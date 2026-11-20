//! `dict[K, V]`: a hash map that remembers insertion order, like Python's.
//!
//! Two arrays, both in collected memory:
//! - `entries`, in insertion order. Each entry is `[tag: i64][key][value]`, with the key and the
//!   value each padded to a multiple of 8 bytes. A tag of 0 marks a removed entry; otherwise it
//!   is the key's hash with the lowest bit set.
//! - `index`, an open-addressing table (linear probing) of entry numbers, with -1 for empty and
//!   -2 for a removed slot. It is kept at most two-thirds full.
//!
//! Hashing and key equality come from the descriptor, as functions the compiler generates for
//! the key type. Generated code finds a value at `entry + 8 + round8(key size)`; that layout must
//! match `dict_value_offset` in `lang_codegen`.

use crate::{Str, TypeDesc, gc};

#[repr(C)]
pub struct Dict {
    pub desc: *const TypeDesc,
    pub len: i64,
    entries: *mut u8,
    used: i64,
    cap: i64,
    index: *mut i64,
    index_cap: i64,
}

const EMPTY: i64 = -1;
const REMOVED: i64 = -2;

fn round8(n: usize) -> usize {
    (n + 7) & !7
}

/// SplitMix64's finaliser: spreads the bits of a hash, so that keys like consecutive integers do
/// not cluster in the table.
fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58476d1ce4e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

impl Dict {
    fn desc(&self) -> &TypeDesc {
        // SAFETY: descriptors are static constants emitted by the compiler.
        unsafe { &*self.desc }
    }

    fn key_size(&self) -> usize {
        self.desc().elem_size as usize
    }

    fn value_offset(&self) -> usize {
        8 + round8(self.key_size())
    }

    fn entry_size(&self) -> usize {
        self.value_offset() + round8(self.desc().value_size as usize)
    }

    unsafe fn entry(&self, i: usize) -> *mut u8 {
        unsafe { self.entries.add(i * self.entry_size()) }
    }

    unsafe fn tag(entry: *const u8) -> i64 {
        unsafe { *(entry as *const i64) }
    }

    /// Whether the keys are `str`, whose hash and equality are done here directly instead of
    /// through the descriptor's functions (docs/09, phase 4): no indirect call per probe, and
    /// the hash inlined.
    fn str_keys(&self) -> bool {
        let str_hash: crate::HashFn = crate::strings::lrt_str_hash_ptr;
        self.desc().hash.is_some_and(|hash| hash as usize == str_hash as usize)
    }

    fn hash(&self, key: *const u8) -> i64 {
        if self.str_keys() {
            // SAFETY: the keys are `str`.
            let key = unsafe { *(key as *const Str) };
            return str_tag(unsafe { key_bytes(key.ptr, key.len) });
        }
        let hash = self.desc().hash.expect("dict descriptor has a hash function");
        // SAFETY: `key` points to a key of this dict's key type.
        (mix(unsafe { hash(key) as u64 }) | 1) as i64
    }

    fn keys_equal(&self, a: *const u8, b: *const u8) -> bool {
        if self.str_keys() {
            // SAFETY: the keys are `str`.
            let (a, b) = unsafe { (*(a as *const Str), *(b as *const Str)) };
            return a.len == b.len
                && (a.ptr == b.ptr
                    || unsafe {
                        std::slice::from_raw_parts(a.ptr, a.len as usize)
                            == std::slice::from_raw_parts(b.ptr, b.len as usize)
                    });
        }
        let eq = self.desc().eq.expect("dict descriptor has an equality function");
        // SAFETY: both point to keys of this dict's key type.
        unsafe { eq(a, b) != 0 }
    }

    /// The index slot for `key`, and the entry number if the key is present. When absent, the
    /// slot is where the key should go (the first removed slot seen, or the empty one).
    unsafe fn probe(&self, key: *const u8, hash: i64) -> (usize, Option<usize>) {
        unsafe { self.probe_by(hash, |stored| self.keys_equal(key, stored)) }
    }

    /// `probe`, comparing keys with `same`, which gets a pointer to a stored key.
    #[inline(always)]
    unsafe fn probe_by(&self, hash: i64, same: impl Fn(*const u8) -> bool) -> (usize, Option<usize>) {
        let mask = self.index_cap as usize - 1;
        let mut i = hash as usize & mask;
        let mut first_removed = None;
        loop {
            let found = unsafe { *self.index.add(i) };
            if found == EMPTY {
                return (first_removed.unwrap_or(i), None);
            }
            if found == REMOVED {
                first_removed.get_or_insert(i);
            } else {
                let entry = unsafe { self.entry(found as usize) };
                if unsafe { Dict::tag(entry) } == hash && same(unsafe { entry.add(8) }) {
                    return (i, Some(found as usize));
                }
            }
            i = (i + 1) & mask;
        }
    }

    unsafe fn find(&self, key: *const u8) -> Option<usize> {
        if self.index_cap == 0 {
            return None;
        }
        unsafe { self.probe(key, self.hash(key)).1 }
    }

    /// Compact the entries (dropping removed ones) into arrays sized for growth.
    unsafe fn rebuild(&mut self) {
        let cap = ((self.len as usize + 1) * 2).max(8);
        let index_cap = (cap * 3 / 2).next_power_of_two();
        let entry_size = self.entry_size();
        let entries = gc::alloc(cap * entry_size);
        let index = gc::alloc_atomic(index_cap * 8) as *mut i64;
        unsafe {
            for i in 0..index_cap {
                *index.add(i) = EMPTY;
            }
            let mut count = 0;
            for i in 0..self.used as usize {
                let from = self.entry(i);
                let tag = Dict::tag(from);
                if tag == 0 {
                    continue;
                }
                std::ptr::copy_nonoverlapping(from, entries.add(count * entry_size), entry_size);
                let mut slot = tag as usize & (index_cap - 1);
                while *index.add(slot) != EMPTY {
                    slot = (slot + 1) & (index_cap - 1);
                }
                *index.add(slot) = count as i64;
                count += 1;
            }
            self.used = count as i64;
        }
        self.entries = entries;
        self.cap = cap as i64;
        self.index = index;
        self.index_cap = index_cap as i64;
    }
}

unsafe fn dict<'a>(d: *mut Dict) -> &'a mut Dict {
    unsafe { &mut *d }
}

unsafe fn key_bytes<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    if len <= 0 { &[] } else { unsafe { std::slice::from_raw_parts(ptr, len as usize) } }
}

/// The tag stored with a `str` key: its hash, odd so that 0 can mark a removed entry. The
/// string hash is already well mixed, so it is not mixed again (docs/08, D90).
#[inline(always)]
fn str_tag(bytes: &[u8]) -> i64 {
    (crate::strings::hash_str(bytes) | 1) as i64
}

/// The tag stored with an `int` key: as the generic path computes it (the generated hash of an
/// `int` is the value itself).
#[inline(always)]
fn int_tag(key: i64) -> i64 {
    (mix(key as u64) | 1) as i64
}

impl Dict {
    /// The entry for a `str` key, if present, and the slot where it is or would go.
    #[inline(always)]
    unsafe fn probe_str(&self, bytes: &[u8], hash: i64) -> (usize, Option<usize>) {
        unsafe {
            self.probe_by(hash, |stored| {
                let stored = *(stored as *const Str);
                stored.len as usize == bytes.len()
                    && (stored.ptr == bytes.as_ptr() || key_bytes(stored.ptr, stored.len) == bytes)
            })
        }
    }

    #[inline(always)]
    unsafe fn probe_int(&self, key: i64, hash: i64) -> (usize, Option<usize>) {
        unsafe { self.probe_by(hash, |stored| *(stored as *const i64) == key) }
    }

    /// Insert a key known to be absent at `slot` (from a probe with `hash`), growing first if
    /// needed, and return the new entry's value pointer. `key` points to the key to copy.
    unsafe fn insert(
        &mut self,
        mut slot: usize,
        hash: i64,
        key: *const u8,
        reprobe: impl Fn(&Self) -> usize,
    ) -> *mut u8 {
        unsafe {
            if self.used == self.cap {
                self.rebuild();
                slot = reprobe(self);
            }
            let i = self.used as usize;
            let entry = self.entry(i);
            *(entry as *mut i64) = hash;
            std::ptr::copy_nonoverlapping(key, entry.add(8), self.key_size());
            *self.index.add(slot) = i as i64;
            self.used += 1;
            self.len += 1;
            entry.add(self.value_offset())
        }
    }
}

/// `lrt_dict_find` for a `str` key, passed as pointer and length: no spill, no check of the
/// key type, and the hash and comparison inlined (docs/10, milestone 4).
///
/// # Safety
/// `d` must be a dict with `str` keys; `ptr` and `len` describe a string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_find_str(d: *mut Dict, ptr: *const u8, len: i64) -> *mut u8 {
    unsafe {
        let d = dict(d);
        if d.index_cap == 0 {
            return std::ptr::null_mut();
        }
        let bytes = key_bytes(ptr, len);
        match d.probe_str(bytes, str_tag(bytes)).1 {
            Some(i) => d.entry(i).add(d.value_offset()),
            None => std::ptr::null_mut(),
        }
    }
}

/// `lrt_dict_slot` for a `str` key, passed as pointer and length.
///
/// # Safety
/// As `lrt_dict_find_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_slot_str(d: *mut Dict, ptr: *const u8, len: i64) -> *mut u8 {
    unsafe {
        let d = dict(d);
        if d.index_cap == 0 {
            d.rebuild();
        }
        let bytes = key_bytes(ptr, len);
        let hash = str_tag(bytes);
        let (slot, found) = d.probe_str(bytes, hash);
        if let Some(i) = found {
            return d.entry(i).add(d.value_offset());
        }
        let key = Str { ptr, len };
        d.insert(slot, hash, &key as *const Str as *const u8, |d| d.probe_str(bytes, hash).0)
    }
}

/// `lrt_dict_find` for an `int` key, passed by value.
///
/// # Safety
/// `d` must be a dict with `int` keys.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_find_int(d: *mut Dict, key: i64) -> *mut u8 {
    unsafe {
        let d = dict(d);
        if d.index_cap == 0 {
            return std::ptr::null_mut();
        }
        match d.probe_int(key, int_tag(key)).1 {
            Some(i) => d.entry(i).add(d.value_offset()),
            None => std::ptr::null_mut(),
        }
    }
}

/// `lrt_dict_slot` for an `int` key, passed by value.
///
/// # Safety
/// `d` must be a dict with `int` keys.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_slot_int(d: *mut Dict, key: i64) -> *mut u8 {
    unsafe {
        let d = dict(d);
        if d.index_cap == 0 {
            d.rebuild();
        }
        let hash = int_tag(key);
        let (slot, found) = d.probe_int(key, hash);
        if let Some(i) = found {
            return d.entry(i).add(d.value_offset());
        }
        d.insert(slot, hash, &key as *const i64 as *const u8, |d| d.probe_int(key, hash).0)
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dict_new(desc: *const TypeDesc) -> *mut Dict {
    let d = gc::alloc(std::mem::size_of::<Dict>()) as *mut Dict;
    // SAFETY: fresh, zeroed allocation of the right size.
    unsafe { (*d).desc = desc };
    d
}

/// Pointer to the value for `*key`, or null.
///
/// # Safety
/// `d` must be a dict and `key` must point to a key of its key type.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_find(d: *mut Dict, key: *const u8) -> *mut u8 {
    unsafe {
        let d = dict(d);
        match d.find(key) {
            Some(i) => d.entry(i).add(d.value_offset()),
            None => std::ptr::null_mut(),
        }
    }
}

/// Pointer to the value for `*key`, inserting the key with a zeroed value if it is absent.
///
/// # Safety
/// `d` must be a dict and `key` must point to a key of its key type.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_slot(d: *mut Dict, key: *const u8) -> *mut u8 {
    unsafe {
        let d = dict(d);
        if d.index_cap == 0 {
            d.rebuild();
        }
        let hash = d.hash(key);
        let (slot, found) = d.probe(key, hash);
        if let Some(i) = found {
            return d.entry(i).add(d.value_offset());
        }
        d.insert(slot, hash, key, |d| d.probe(key, hash).0)
    }
}

/// The equality function for the values of a set, which has none: any two are equal.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_unit_eq_ptr(_a: *const u8, _b: *const u8) -> i64 {
    1
}

/// Remove `*key`. Returns 1 and copies the value to `out` (if not null) when it was present.
///
/// # Safety
/// `d` must be a dict, `key` must point to a key of its key type, and `out` must be null or
/// have room for a value.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_remove(d: *mut Dict, key: *const u8, out: *mut u8) -> i64 {
    unsafe {
        let d = dict(d);
        if d.index_cap == 0 {
            return 0;
        }
        let (slot, found) = d.probe(key, d.hash(key));
        let Some(i) = found else { return 0 };
        let entry = d.entry(i);
        if !out.is_null() {
            std::ptr::copy_nonoverlapping(entry.add(d.value_offset()), out, d.desc().value_size as usize);
        }
        // Zero the whole entry: the tag marks it removed, and the collector stops seeing the
        // old key and value.
        std::ptr::write_bytes(entry, 0, d.entry_size());
        *d.index.add(slot) = REMOVED;
        d.len -= 1;
        1
    }
}

/// The next live entry at or after `*cursor`, advancing the cursor; null at the end. Generated
/// code reads the key at `entry + 8` and the value at `entry + 8 + round8(key size)`.
///
/// # Safety
/// `d` must be a dict and `cursor` must point to a cursor that started at 0.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_next(d: *mut Dict, cursor: *mut i64) -> *mut u8 {
    unsafe {
        let d = dict(d);
        while *cursor < d.used {
            let entry = d.entry(*cursor as usize);
            *cursor += 1;
            if Dict::tag(entry) != 0 {
                return entry;
            }
        }
        std::ptr::null_mut()
    }
}

/// # Safety
/// `d` must be a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_clear(d: *mut Dict) {
    unsafe {
        let d = dict(d);
        d.len = 0;
        d.used = 0;
        d.cap = 0;
        d.entries = std::ptr::null_mut();
        d.index = std::ptr::null_mut();
        d.index_cap = 0;
    }
}

/// A new dict with the same entries, in the same order.
///
/// # Safety
/// `d` must be a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_copy(d: *mut Dict) -> *mut Dict {
    unsafe {
        let copy = lrt_dict_new((*d).desc);
        let (key_size, value_offset, value_size) = {
            let source = dict(d);
            (source.key_size(), source.value_offset(), source.desc().value_size as usize)
        };
        let mut cursor = 0i64;
        loop {
            let entry = lrt_dict_next(d, &mut cursor);
            if entry.is_null() {
                break;
            }
            let _ = key_size;
            let slot = lrt_dict_slot(copy, entry.add(8));
            std::ptr::copy_nonoverlapping(entry.add(value_offset), slot, value_size);
        }
        copy
    }
}

/// Whether two dicts have the same keys with equal values (in any order). `value_eq` compares
/// two values given pointers to them.
///
/// # Safety
/// Both must be dicts of the same type, and `value_eq` must compare their values.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dict_eq(a: *mut Dict, b: *mut Dict, value_eq: crate::EqFn) -> i64 {
    unsafe {
        if (*a).len != (*b).len {
            return 0;
        }
        let value_offset = dict(a).value_offset();
        let mut cursor = 0i64;
        loop {
            let entry = lrt_dict_next(a, &mut cursor);
            if entry.is_null() {
                return 1;
            }
            let other = lrt_dict_find(b, entry.add(8));
            if other.is_null() || value_eq(entry.add(value_offset), other) == 0 {
                return 0;
            }
        }
    }
}
