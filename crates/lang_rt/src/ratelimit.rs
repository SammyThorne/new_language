//! Rate limiting (docs/14, milestone 14): a token bucket per key.
//!
//! A [`Limiter`] refills `burst` tokens at one per `interval`; a request takes one, and is
//! refused when none is left. Keys are spread over [`SHARDS`] shards by a hash that is random
//! per limiter, each a `Mutex` of a map from key to bucket, so a request costs one lock of one
//! shard, one lookup and some arithmetic. Memory is bounded:
//!
//! - a bucket that has refilled to `burst` says nothing a missing one would not, so each shard
//!   drops those every minute, when a request next reaches it;
//! - a shard holds at most [`MAX_KEYS`] / [`SHARDS`] keys; a new key past that first drops the
//!   full buckets, then the eighth of the shard that was used longest ago, so an attacker
//!   cycling addresses costs a bounded table and a scan now and then;
//! - keys longer than [`MAX_KEY_BYTES`] are kept as a hash.
//!
//! Time comes from the monotonic clock, or from `datetime.set_clock_for_tests` when that is
//! set, so tests can step it. Limits are per process.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use crate::Str;

pub(crate) const SHARDS: usize = 64;
/// Keys kept by one limiter at most.
pub(crate) const MAX_KEYS: usize = 100_000;
const SHARD_KEYS: usize = MAX_KEYS / SHARDS;
const MAX_KEY_BYTES: usize = 128;
const SWEEP_EVERY: i64 = 60_000_000_000;

/// What a request is told.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Decision {
    /// Allowed: the tokens left after this one, and the seconds until the bucket is full.
    Allowed { available: u64, full_in: u64 },
    /// Refused: the seconds until a token is there.
    Refused { retry_after: u64 },
}

struct Bucket {
    /// Tokens at `at`; below zero after a lock-out.
    tokens: f64,
    /// When it was last used, in nanoseconds on [`now`]'s clock.
    at: i64,
}

#[derive(Default)]
struct Shard {
    buckets: HashMap<Box<str>, Bucket>,
    swept: i64,
}

pub(crate) struct Limiter {
    /// Nanoseconds per token.
    interval: f64,
    burst: f64,
    hasher: RandomState,
    shards: Box<[Mutex<Shard>]>,
}

/// Nanoseconds on the limiter's clock: the fixed test clock if there is one, or the monotonic
/// clock.
pub(crate) fn now() -> i64 {
    if let Some(fixed) = crate::datetime::fixed_now() {
        return fixed;
    }
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    let start = *START.get_or_init(std::time::Instant::now);
    // Never 0, so a new shard sweeps a minute after its first request.
    i64::try_from(start.elapsed().as_nanos()).unwrap_or(i64::MAX).max(1)
}

fn seconds(nanos: f64) -> u64 {
    (nanos / 1e9).ceil().max(1.0) as u64
}

impl Limiter {
    /// `burst` tokens, refilled one every `interval` nanoseconds.
    pub(crate) fn new(interval: u64, burst: u64) -> Limiter {
        Limiter {
            interval: interval.max(1) as f64,
            burst: burst.max(1) as f64,
            hasher: RandomState::new(),
            shards: (0..SHARDS).map(|_| Mutex::new(Shard::default())).collect(),
        }
    }

    fn key<'a>(&self, key: &'a str) -> std::borrow::Cow<'a, str> {
        if key.len() <= MAX_KEY_BYTES {
            return std::borrow::Cow::Borrowed(key);
        }
        let hash = self.hasher.hash_one(key);
        let mut cut = MAX_KEY_BYTES - 17;
        while !key.is_char_boundary(cut) {
            cut -= 1;
        }
        std::borrow::Cow::Owned(format!("{}#{hash:016x}", &key[..cut]))
    }

    fn shard(&self, key: &str) -> std::sync::MutexGuard<'_, Shard> {
        let index = (self.hasher.hash_one(key) % SHARDS as u64) as usize;
        self.shards[index].lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The tokens a bucket has at `now`.
    fn level(&self, bucket: &Bucket, now: i64) -> f64 {
        let elapsed = now.saturating_sub(bucket.at).max(0) as f64;
        (bucket.tokens + elapsed / self.interval).min(self.burst)
    }

    /// Take a token for `key`, if there is one.
    pub(crate) fn take(&self, key: &str, now: i64) -> Decision {
        let key = self.key(key);
        let mut shard = self.shard(&key);
        self.sweep(&mut shard, now);
        let tokens = match shard.buckets.get(&*key) {
            Some(bucket) => self.level(bucket, now),
            None => {
                self.make_room(&mut shard, now);
                self.burst
            }
        };
        if tokens < 1.0 {
            let retry = (1.0 - tokens) * self.interval;
            return Decision::Refused { retry_after: seconds(retry) };
        }
        let tokens = tokens - 1.0;
        let full_in = (self.burst - tokens) * self.interval;
        shard.buckets.insert(key.into_owned().into_boxed_str(), Bucket { tokens, at: now });
        Decision::Allowed { available: tokens.floor() as u64, full_in: seconds(full_in) }
    }

    /// The seconds until `key` has a token again, if it has none now; takes nothing.
    pub(crate) fn waiting(&self, key: &str, now: i64) -> Option<u64> {
        let key = self.key(key);
        let shard = self.shard(&key);
        let tokens = self.level(shard.buckets.get(&*key)?, now);
        (tokens < 1.0).then(|| seconds((1.0 - tokens) * self.interval))
    }

    /// Empty `key`'s bucket and then some, so it gets its next token only after a whole
    /// `burst` of intervals.
    pub(crate) fn lock_out(&self, key: &str, now: i64) {
        let key = self.key(key);
        let mut shard = self.shard(&key);
        if !shard.buckets.contains_key(&*key) {
            self.make_room(&mut shard, now);
        }
        shard.buckets.insert(key.into_owned().into_boxed_str(), Bucket { tokens: 1.0 - self.burst, at: now });
    }

    /// Drop the full buckets once a minute.
    fn sweep(&self, shard: &mut Shard, now: i64) {
        if shard.swept == 0 {
            shard.swept = now;
        } else if now.saturating_sub(shard.swept) >= SWEEP_EVERY {
            shard.swept = now;
            shard.buckets.retain(|_, b| self.level(b, now) < self.burst);
        }
    }

    /// Room for one more key in a full shard: drop the full buckets, and if that is not enough,
    /// the eighth used longest ago.
    fn make_room(&self, shard: &mut Shard, now: i64) {
        if shard.buckets.len() < SHARD_KEYS {
            return;
        }
        shard.buckets.retain(|_, b| self.level(b, now) < self.burst);
        if shard.buckets.len() < SHARD_KEYS {
            return;
        }
        let mut ages: Vec<i64> = shard.buckets.values().map(|b| b.at).collect();
        let cut = SHARD_KEYS / 8;
        let (_, oldest, _) = ages.select_nth_unstable(cut);
        let oldest = *oldest;
        let mut dropped = 0;
        shard.buckets.retain(|_, b| {
            let keep = b.at > oldest || dropped > cut;
            if !keep {
                dropped += 1;
            }
            keep
        });
    }

    /// Keys held, over every shard.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().unwrap_or_else(|p| p.into_inner()).buckets.len()).sum()
    }
}

// ---- for compiled code ------------------------------------------------------------------

static LIMITERS: RwLock<Vec<Arc<Limiter>>> = RwLock::new(Vec::new());

/// A new limiter of `burst` tokens refilled one every `interval_ns`; returns its id.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_rate_new(interval_ns: i64, burst: i64) -> i64 {
    let limiter = Arc::new(Limiter::new(interval_ns.max(1) as u64, burst.max(1) as u64));
    let mut all = LIMITERS.write().unwrap_or_else(|p| p.into_inner());
    all.push(limiter);
    all.len() as i64 - 1
}

/// Take a token for `key` from limiter `id`. Allowed: the tokens left times 2^32 plus the
/// seconds until the bucket is full (0 or more). Refused: minus the seconds until a token is
/// there.
///
/// # Safety
/// `key` and `key_len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_rate_take(id: i64, key: *const u8, key_len: i64) -> i64 {
    let key = unsafe { Str::from_raw(key, key_len) };
    let Some(limiter) = LIMITERS.read().unwrap_or_else(|p| p.into_inner()).get(id.max(0) as usize).cloned() else {
        return 0;
    };
    match limiter.take(key, now()) {
        Decision::Allowed { available, full_in } => {
            ((available.min(u32::MAX as u64 >> 1) as i64) << 32) | full_in.min(u32::MAX as u64) as i64
        }
        Decision::Refused { retry_after } => -(retry_after.clamp(1, i64::MAX as u64) as i64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    const SECOND: i64 = 1_000_000_000;

    #[test]
    fn burst_then_refusal_then_refill() {
        // 2 per second, a burst of 4.
        let limiter = Limiter::new(500_000_000, 4);
        let t = 10 * SECOND;
        for left in (0..4).rev() {
            assert!(matches!(limiter.take("a", t), Decision::Allowed { available, .. } if available == left));
        }
        assert_eq!(limiter.take("a", t), Decision::Refused { retry_after: 1 });
        // Other keys have their own bucket.
        assert!(matches!(limiter.take("b", t), Decision::Allowed { available: 3, full_in: 1 }));
        // Half a second brings one token back.
        assert!(matches!(limiter.take("a", t + SECOND / 2), Decision::Allowed { available: 0, full_in: 2 }));
        assert!(matches!(limiter.take("a", t + SECOND / 2), Decision::Refused { .. }));
        // Two seconds fill it.
        assert!(matches!(limiter.take("a", t + 3 * SECOND), Decision::Allowed { available: 3, .. }));
    }

    #[test]
    fn lock_out_waits_a_whole_burst() {
        // As the dashboard's: 10 a minute.
        let limiter = Limiter::new(6 * SECOND as u64, 10);
        let t = SECOND;
        assert_eq!(limiter.waiting("x", t), None);
        limiter.lock_out("x", t);
        assert_eq!(limiter.waiting("x", t), Some(60));
        assert_eq!(limiter.waiting("x", t + 59 * SECOND), Some(1));
        assert_eq!(limiter.waiting("x", t + 60 * SECOND), None);
    }

    #[test]
    fn keys_are_capped() {
        let limiter = Limiter::new(SECOND as u64, 5);
        let mut t = SECOND;
        for i in 0..200_000 {
            limiter.take(&format!("10.{}.{}.{}", i >> 16, (i >> 8) & 255, i & 255), t);
            t += 1000;
        }
        let held = limiter.len();
        assert!(held <= MAX_KEYS && held > MAX_KEYS / 2, "{held}");
        // The tables stay as large as the cap needs, not as the keys seen.
        let capacity: usize = limiter.shards.iter().map(|s| s.lock().unwrap().buckets.capacity()).sum();
        assert!(capacity < 3 * MAX_KEYS, "{capacity}");
        // The latest key is still limited; the first was evicted.
        assert!(matches!(limiter.take("10.3.13.63", t), Decision::Allowed { available: 3, .. }));
        assert!(matches!(limiter.take("10.0.0.0", t), Decision::Allowed { available: 4, .. }));
    }

    #[test]
    fn idle_keys_are_swept() {
        let limiter = Limiter::new(SECOND as u64, 2);
        for i in 0..1000 {
            limiter.take(&i.to_string(), SECOND);
        }
        assert_eq!(limiter.len(), 1000);
        // A minute later every bucket is full again; requests sweep the shards they reach.
        for i in 0..1000 {
            limiter.take(&format!("late {i}"), 62 * SECOND);
        }
        assert_eq!(limiter.len(), 1000);
    }

    #[test]
    fn long_keys_are_hashed() {
        let limiter = Limiter::new(SECOND as u64, 1);
        let long = "k".repeat(10_000);
        assert!(matches!(limiter.take(&long, SECOND), Decision::Allowed { .. }));
        assert!(matches!(limiter.take(&long, SECOND), Decision::Refused { .. }));
        assert!(matches!(limiter.take(&"k".repeat(9_999), SECOND), Decision::Allowed { .. }));
        let shard_keys: usize =
            limiter.shards.iter().map(|s| s.lock().unwrap().buckets.keys().map(|k| k.len()).sum::<usize>()).sum();
        assert!(shard_keys <= 2 * MAX_KEY_BYTES);
    }

    #[test]
    fn no_more_than_the_burst_out_of_many_at_once() {
        let limiter = Arc::new(Limiter::new(1_000 * SECOND as u64, 50));
        let allowed = Arc::new(AtomicI64::new(0));
        let threads: Vec<_> = (0..16)
            .map(|_| {
                let (limiter, allowed) = (limiter.clone(), allowed.clone());
                std::thread::spawn(move || {
                    for _ in 0..1000 / 16 + 1 {
                        if matches!(limiter.take("same", SECOND), Decision::Allowed { .. }) {
                            allowed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(allowed.load(Ordering::Relaxed), 50);
    }
}
