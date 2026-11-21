//! Dates, times and time zones for the `datetime` module.
//!
//! The module keeps its values in the language; this file does the parts that work on text or files:
//! the clock, parsing and formatting, Go-syntax durations, and TZif time zones (RFC 8536,
//! versions 1 to 3, including the footer's POSIX TZ rule for instants past the table).
//!
//! An instant is whole seconds since the Unix epoch plus nanoseconds; an offset is seconds
//! east of UTC. Calendar dates are proleptic Gregorian.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use crate::Str;
use crate::sys::record_error;

const DAY: i64 = 86_400;

// ---- the clock ----------------------------------------------------------------------------

static FIXED: AtomicBool = AtomicBool::new(false);
static FIXED_NS: AtomicI64 = AtomicI64::new(0);

/// Nanoseconds since the Unix epoch, or the instant `lrt_dt_set_clock` fixed.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dt_now_ns() -> i64 {
    if FIXED.load(Ordering::Acquire) {
        return FIXED_NS.load(Ordering::Acquire);
    }
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}

/// The instant `lrt_dt_set_clock` fixed, if it is fixed.
pub(crate) fn fixed_now() -> Option<i64> {
    FIXED.load(Ordering::Acquire).then(|| FIXED_NS.load(Ordering::Acquire))
}

/// Make `now()` return `ns` (when `on` is 1), or the system clock again (0).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dt_set_clock(ns: i64, on: i64) {
    FIXED_NS.store(ns, Ordering::Release);
    FIXED.store(on != 0, Ordering::Release);
}

// ---- calendar -----------------------------------------------------------------------------

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day number.
pub(crate) fn civil(days: i64) -> (i64, i64, i64) {
    let (y, m, d) = crate::http::civil(days);
    (y, i64::from(m), i64::from(d))
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn month_days(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// 0 for Monday up to 6 for Sunday.
fn weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

/// Days since the epoch of a date, or 0 after recording why it is not one. Years run from 1
/// to 9999.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dt_days(year: i64, month: i64, day: i64) -> i64 {
    match check_date(year, month, day) {
        Ok(()) => days_from_civil(year, month, day),
        Err(message) => {
            record_error(message);
            0
        }
    }
}

fn check_date(year: i64, month: i64, day: i64) -> Result<(), String> {
    if !(1..=9999).contains(&year) {
        return Err(format!("year {year} is out of range (1 to 9999)"));
    }
    if !(1..=12).contains(&month) {
        return Err(format!("month {month} is out of range (1 to 12)"));
    }
    let last = month_days(year, month);
    if !(1..=last).contains(&day) {
        return Err(format!("day {day} is out of range for {year:04}-{month:02} (1 to {last})"));
    }
    Ok(())
}

/// One part of the civil date of a day number: 0 year, 1 month, 2 day, 3 weekday (0 is
/// Monday), 4 day of the year (1 is January 1st).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dt_civil(days: i64, part: i64) -> i64 {
    let (y, m, d) = civil(days);
    match part {
        0 => y,
        1 => m,
        2 => d,
        3 => weekday(days),
        _ => days - days_from_civil(y, 1, 1) + 1,
    }
}

// ---- parsing ------------------------------------------------------------------------------

thread_local! {
    /// Whether the last `lrt_dt_parse` succeeded, and its nanoseconds and offset.
    static PARSED: Cell<(bool, i64, i64)> = const { Cell::new((false, 0, 0)) };
}

struct Cursor<'a> {
    text: &'a [u8],
    at: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn unexpected(&self, wanted: &str) -> String {
        let shown = String::from_utf8_lossy(self.text);
        match std::str::from_utf8(&self.text[self.at..]).ok().and_then(|s| s.chars().next()) {
            Some(c) => format!("unexpected {c:?} at position {} in {shown:?}: expected {wanted}", self.at + 1),
            None if self.at < self.text.len() => format!("unexpected byte at position {} in {shown:?}", self.at + 1),
            None => format!("{shown:?} ends early: expected {wanted}"),
        }
    }

    fn digits(&mut self, count: usize, what: &str) -> Result<i64, String> {
        let mut value = 0;
        for _ in 0..count {
            match self.peek() {
                Some(b @ b'0'..=b'9') => {
                    value = value * 10 + i64::from(b - b'0');
                    self.at += 1;
                }
                _ => return Err(self.unexpected(what)),
            }
        }
        Ok(value)
    }

    fn expect(&mut self, byte: u8, what: &str) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.unexpected(what))
        }
    }

    fn end(&self) -> Result<(), String> {
        if self.at == self.text.len() { Ok(()) } else { Err(self.unexpected("the end of the text")) }
    }
}

fn parse_date_part(c: &mut Cursor) -> Result<i64, String> {
    let year = c.digits(4, "a four-digit year")?;
    c.expect(b'-', "`-`")?;
    let month = c.digits(2, "a two-digit month")?;
    c.expect(b'-', "`-`")?;
    let day = c.digits(2, "a two-digit day")?;
    check_date(year, month, day)?;
    Ok(days_from_civil(year, month, day))
}

/// Parse RFC 3339 and the ISO 8601 forms `datetime.parse` documents: returns (seconds,
/// nanoseconds, offset).
fn parse_datetime(text: &[u8]) -> Result<(i64, i64, i64), String> {
    let mut c = Cursor { text, at: 0 };
    let days = parse_date_part(&mut c)?;
    match c.peek() {
        Some(b'T' | b't' | b' ') => c.at += 1,
        _ => return Err(c.unexpected("`T` or a space between the date and the time")),
    }
    let hour = c.digits(2, "a two-digit hour")?;
    c.expect(b':', "`:`")?;
    let minute = c.digits(2, "two-digit minutes")?;
    let mut second = 0;
    let mut nano = 0;
    if c.peek() == Some(b':') {
        c.at += 1;
        second = c.digits(2, "two-digit seconds")?;
        if matches!(c.peek(), Some(b'.' | b',')) {
            c.at += 1;
            let start = c.at;
            while let Some(b @ b'0'..=b'9') = c.peek() {
                if c.at - start == 9 {
                    return Err(c.unexpected("at most 9 digits of fractional seconds"));
                }
                nano = nano * 10 + i64::from(b - b'0');
                c.at += 1;
            }
            if c.at == start {
                return Err(c.unexpected("digits of fractional seconds"));
            }
            nano *= 10i64.pow(9 - (c.at - start) as u32);
        }
    }
    if hour > 23 {
        return Err(format!("hour {hour} is out of range (0 to 23)"));
    }
    if minute > 59 {
        return Err(format!("minute {minute} is out of range (0 to 59)"));
    }
    // A leap second, 60, is read as the first second of the next minute.
    if second > 60 {
        return Err(format!("second {second} is out of range (0 to 59)"));
    }
    let offset = match c.peek() {
        Some(b'Z' | b'z') => {
            c.at += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            c.at += 1;
            let hours = c.digits(2, "a two-digit offset hour")?;
            let mut minutes = 0;
            let mut seconds = 0;
            if c.peek() == Some(b':') {
                c.at += 1;
                minutes = c.digits(2, "two-digit offset minutes")?;
                if c.peek() == Some(b':') {
                    c.at += 1;
                    seconds = c.digits(2, "two-digit offset seconds")?;
                }
            } else if matches!(c.peek(), Some(b'0'..=b'9')) {
                minutes = c.digits(2, "two-digit offset minutes")?;
            }
            if hours > 23 || minutes > 59 || seconds > 59 {
                return Err(format!("the offset in {:?} is out of range", String::from_utf8_lossy(text)));
            }
            let total = hours * 3600 + minutes * 60 + seconds;
            if sign == b'-' { -total } else { total }
        }
        _ => return Err(c.unexpected("`Z` or an offset such as `+01:00`")),
    };
    c.end()?;
    let local = days * DAY + hour * 3600 + minute * 60 + second;
    Ok((local - offset, nano, offset))
}

/// Parse a date and time; returns the seconds, with the nanoseconds and offset kept for
/// `lrt_dt_parsed`. On failure records the error and returns 0.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dt_parse(ptr: *const u8, len: i64) -> i64 {
    let text = unsafe { Str::from_raw(ptr, len) };
    match parse_datetime(text.as_bytes()) {
        Ok((seconds, nano, offset)) => {
            PARSED.set((true, nano, offset));
            seconds
        }
        Err(message) => {
            PARSED.set((false, 0, 0));
            record_error(message);
            0
        }
    }
}

/// From the last `lrt_dt_parse` on this thread: its nanoseconds (0), its offset (1), or 1 if it
/// succeeded (2).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dt_parsed(part: i64) -> i64 {
    let (ok, nano, offset) = PARSED.get();
    match part {
        0 => nano,
        1 => offset,
        _ => i64::from(ok),
    }
}

/// Parse `YYYY-MM-DD` into a day number, or record the error and return 0.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dt_parse_date(ptr: *const u8, len: i64) -> i64 {
    let text = unsafe { Str::from_raw(ptr, len) };
    let mut c = Cursor { text: text.as_bytes(), at: 0 };
    match parse_date_part(&mut c).and_then(|days| c.end().map(|()| days)) {
        Ok(days) => days,
        Err(message) => {
            record_error(message);
            0
        }
    }
}

// ---- durations ----------------------------------------------------------------------------

/// Parse a duration in Go's syntax: an optional sign, then numbers with units, as in
/// `"1h30m"`, `"1.5s"` or `"-300ms"`. The units are `ns`, `us` (or `µs`), `ms`, `s`, `m`, `h`.
fn parse_duration(text: &str) -> Result<i64, String> {
    let quoted = format!("{text:?}");
    let invalid = |why: &str| format!("invalid duration {quoted}: {why}");
    let mut rest = text;
    let negative = rest.starts_with('-');
    if negative || rest.starts_with('+') {
        rest = &rest[1..];
    }
    if rest == "0" {
        return Ok(0);
    }
    if rest.is_empty() {
        return Err(invalid("expected a number and a unit, such as `30s` or `1h30m`"));
    }
    let mut total: i128 = 0;
    while !rest.is_empty() {
        let whole_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let whole = &rest[..whole_end];
        rest = &rest[whole_end..];
        let mut fraction = "";
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
            fraction = &after[..end];
            rest = &after[end..];
        }
        if whole.is_empty() && fraction.is_empty() {
            return Err(invalid("expected a number"));
        }
        let unit_end = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let unit: i128 = match &rest[..unit_end] {
            "ns" => 1,
            "us" | "µs" | "μs" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60_000_000_000,
            "h" => 3_600_000_000_000,
            "" => return Err(invalid("a number needs a unit (ns, us, ms, s, m or h)")),
            other => return Err(invalid(&format!("unknown unit `{other}` (use ns, us, ms, s, m or h)"))),
        };
        rest = &rest[unit_end..];
        let whole: i128 = if whole.is_empty() { 0 } else { whole.parse().map_err(|_| invalid("too large"))? };
        let mut value = whole.checked_mul(unit).ok_or_else(|| invalid("too large"))?;
        let mut scale = unit;
        for digit in fraction.bytes() {
            scale /= 10;
            if scale == 0 {
                break;
            }
            value += i128::from(digit - b'0') * scale;
        }
        total += value;
        if total > i128::from(i64::MAX) + 1 {
            return Err(invalid("too large"));
        }
    }
    let total = if negative { -total } else { total };
    i64::try_from(total).map_err(|_| invalid("too large"))
}

/// Parse a Go-syntax duration into nanoseconds, or record the error and return 0.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dt_parse_duration(ptr: *const u8, len: i64) -> i64 {
    let text = unsafe { Str::from_raw(ptr, len) };
    match parse_duration(text) {
        Ok(ns) => ns,
        Err(message) => {
            record_error(message);
            0
        }
    }
}

/// A duration as Go prints it: `"1h30m0s"`, `"1.5s"`, `"300ms"`, `"0s"`.
fn format_duration(ns: i64) -> String {
    if ns == 0 {
        return "0s".into();
    }
    let mut out = String::new();
    if ns < 0 {
        out.push('-');
    }
    let u = ns.unsigned_abs();
    if u < 1_000_000_000 {
        let (scale, unit) = match u {
            0..1_000 => (1, "ns"),
            1_000..1_000_000 => (1_000, "µs"),
            _ => (1_000_000, "ms"),
        };
        push_decimal(&mut out, u, scale);
        out.push_str(unit);
        return out;
    }
    let hours = u / 3_600_000_000_000;
    let minutes = u / 60_000_000_000 % 60;
    let rest = u % 60_000_000_000;
    if hours > 0 {
        let _ = write!(out, "{hours}h");
    }
    if hours > 0 || minutes > 0 {
        let _ = write!(out, "{minutes}m");
    }
    push_decimal(&mut out, rest, 1_000_000_000);
    out.push('s');
    out
}

/// `value / scale` with the fraction's trailing zeros left off.
fn push_decimal(out: &mut String, value: u64, scale: u64) {
    let _ = write!(out, "{}", value / scale);
    let fraction = value % scale;
    if fraction > 0 {
        let width = scale.ilog10() as usize;
        let digits = format!("{fraction:0width$}");
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dt_duration_str(ns: i64) -> Str {
    Str::from_bytes(format_duration(ns).as_bytes())
}

// ---- formatting ---------------------------------------------------------------------------

const DAY_NAMES: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn push_offset(out: &mut String, offset: i64, colon: bool) {
    let sign = if offset < 0 { '-' } else { '+' };
    let minutes = offset.abs() / 60;
    let sep = if colon { ":" } else { "" };
    let _ = write!(out, "{sign}{:02}{sep}{:02}", minutes / 60, minutes % 60);
}

/// RFC 3339: `2026-11-20T09:30:00Z`, `2026-11-20T09:30:00.25+01:00`.
fn format_iso(seconds: i64, nano: i64, offset: i64) -> String {
    let local = seconds + offset;
    let days = local.div_euclid(DAY);
    let time = local.rem_euclid(DAY);
    let (y, m, d) = civil(days);
    let mut out = String::with_capacity(32);
    let _ = write!(out, "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", time / 3600, time / 60 % 60, time % 60);
    if nano > 0 {
        let digits = format!("{nano:09}");
        out.push('.');
        out.push_str(digits.trim_end_matches('0'));
    }
    if offset == 0 {
        out.push('Z');
    } else {
        push_offset(&mut out, offset, true);
    }
    out
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_dt_format_iso(seconds: i64, nano: i64, offset: i64) -> Str {
    Str::from_bytes(format_iso(seconds, nano, offset).as_bytes())
}

/// The strftime subset `datetime` documents, or an error naming the directive it does not know.
fn format_pattern(seconds: i64, nano: i64, offset: i64, abbrev: &str, pattern: &str) -> Result<String, String> {
    let local = seconds + offset;
    let days = local.div_euclid(DAY);
    let time = local.rem_euclid(DAY);
    let (y, m, d) = civil(days);
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let _ = match chars.next() {
            Some('Y') => write!(out, "{y:04}"),
            Some('m') => write!(out, "{m:02}"),
            Some('d') => write!(out, "{d:02}"),
            Some('H') => write!(out, "{:02}", time / 3600),
            Some('M') => write!(out, "{:02}", time / 60 % 60),
            Some('S') => write!(out, "{:02}", time % 60),
            Some('f') => write!(out, "{:06}", nano / 1000),
            Some('z') => {
                push_offset(&mut out, offset, false);
                Ok(())
            }
            Some('Z') => write!(out, "{abbrev}"),
            Some('a') => write!(out, "{}", &DAY_NAMES[weekday(days) as usize][..3]),
            Some('A') => write!(out, "{}", DAY_NAMES[weekday(days) as usize]),
            Some('b') => write!(out, "{}", &MONTH_NAMES[m as usize - 1][..3]),
            Some('B') => write!(out, "{}", MONTH_NAMES[m as usize - 1]),
            Some('j') => write!(out, "{:03}", days - days_from_civil(y, 1, 1) + 1),
            Some('%') => write!(out, "%"),
            Some(other) => {
                return Err(format!(
                    "unknown directive `%{other}` in the format {pattern:?}; the directives are \
                     %Y %m %d %H %M %S %f %z %Z %a %A %b %B %j and %%"
                ));
            }
            None => return Err(format!("the format {pattern:?} ends with a lone `%`; write `%%` for a percent sign")),
        };
    }
    Ok(out)
}

/// Format an instant with a pattern, or record the error and return `""`.
///
/// # Safety
/// The pointer and length pairs describe UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_dt_format(
    seconds: i64,
    nano: i64,
    offset: i64,
    abbrev: *const u8,
    abbrev_len: i64,
    pattern: *const u8,
    pattern_len: i64,
) -> Str {
    let abbrev = unsafe { Str::from_raw(abbrev, abbrev_len) };
    let pattern = unsafe { Str::from_raw(pattern, pattern_len) };
    match format_pattern(seconds, nano, offset, abbrev, pattern) {
        Ok(text) => Str::from_bytes(text.as_bytes()),
        Err(message) => {
            record_error(message);
            Str::from_bytes(b"")
        }
    }
}

// ---- time zones ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct LocalType {
    offset: i64,
    dst: bool,
    abbrev: String,
}

/// When a POSIX TZ rule starts or ends daylight saving time, in local time.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RuleDay {
    /// `Jn`: day 1 to 365, never counting February 29th.
    Julian(i64),
    /// `n`: day 0 to 365, counting February 29th.
    Ordinal(i64),
    /// `Mm.w.d`: weekday `d` (0 is Sunday) of week `w` (5 is the last) of month `m`.
    Weekday(i64, i64, i64),
}

/// A day of the year and the local time, in seconds into that day, when a change happens.
type Change = (RuleDay, i64);

#[derive(Clone, Debug, PartialEq)]
struct Rule {
    std: LocalType,
    /// Daylight saving time: its type, and when it starts and ends.
    dst: Option<(LocalType, Change, Change)>,
}

#[derive(Debug)]
struct Zone {
    name: String,
    /// Transition instants, ascending, and the type each one starts.
    transitions: Vec<(i64, usize)>,
    types: Vec<LocalType>,
    footer: Option<Rule>,
}

impl RuleDay {
    /// Day of the year, from 0.
    fn day_of_year(self, year: i64) -> i64 {
        match self {
            RuleDay::Julian(n) => n - 1 + i64::from(is_leap(year) && n >= 60),
            RuleDay::Ordinal(n) => n,
            RuleDay::Weekday(m, w, d) => {
                let first = days_from_civil(year, m, 1);
                // `weekday` counts from Monday; POSIX from Sunday.
                let first_weekday = (weekday(first) + 1) % 7;
                let mut day = 1 + (d - first_weekday).rem_euclid(7) + (w - 1) * 7;
                while day > month_days(year, m) {
                    day -= 7;
                }
                first + day - 1 - days_from_civil(year, 1, 1)
            }
        }
    }
}

impl Rule {
    fn at(&self, t: i64) -> &LocalType {
        let Some((dst, (start_day, start_time), (end_day, end_time))) = &self.dst else {
            return &self.std;
        };
        let year = civil((t + self.std.offset).div_euclid(DAY)).0;
        let jan1 = days_from_civil(year, 1, 1) * DAY;
        let start = jan1 + start_day.day_of_year(year) * DAY + start_time - self.std.offset;
        let end = jan1 + end_day.day_of_year(year) * DAY + end_time - dst.offset;
        let in_dst = if start < end { start <= t && t < end } else { !(end <= t && t < start) };
        if in_dst { dst } else { &self.std }
    }
}

impl Zone {
    fn at(&self, t: i64) -> &LocalType {
        let count = self.transitions.partition_point(|&(at, _)| at <= t);
        match (count, &self.footer) {
            (0, Some(footer)) if self.transitions.is_empty() => footer.at(t),
            (0, _) => &self.types[0],
            (n, Some(footer)) if n == self.transitions.len() => footer.at(t),
            (n, _) => &self.types[self.transitions[n - 1].1],
        }
    }

    /// The offset to use for a wall-clock time: the earlier instant when the time happens twice
    /// (as clocks go back), and the offset from before a gap when it never happens (as clocks
    /// go forward), which moves the time forward by the size of the gap.
    fn resolve(&self, local: i64) -> i64 {
        let early = self.at(local - DAY).offset;
        let late = self.at(local + DAY).offset;
        let valid = |offset: i64| self.at(local - offset).offset == offset;
        match (valid(early), valid(late)) {
            (true, true) => early.max(late),
            (true, false) => early,
            (false, true) => late,
            (false, false) => early,
        }
    }
}

fn be32(data: &[u8], at: usize) -> Option<i64> {
    Some(i64::from(i32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?)))
}

fn be64(data: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// Read a TZif file (RFC 8536).
fn parse_tzif(name: &str, data: &[u8]) -> Result<Zone, String> {
    let bad = || format!("time zone {name:?}: not a valid TZif file");
    if data.get(..4) != Some(b"TZif") {
        return Err(bad());
    }
    let version = data[4];
    let counts = |at: usize| -> Option<[usize; 6]> {
        let mut c = [0usize; 6];
        for (i, slot) in c.iter_mut().enumerate() {
            *slot = usize::try_from(be32(data, at + 20 + i * 4)?).ok()?;
        }
        Some(c)
    };
    let [isut, isstd, leap, time, typ, chars] = counts(0).ok_or_else(bad)?;
    let v1_len = time * 5 + typ * 6 + chars + leap * 8 + isstd + isut;
    let (start, wide) = if version >= b'2' {
        let second = 44 + v1_len;
        if data.get(second..second + 4) != Some(b"TZif") {
            return Err(bad());
        }
        (second, true)
    } else {
        (0, false)
    };
    let [isut, isstd, leap, time, typ, chars] = counts(start).ok_or_else(bad)?;
    if typ == 0 {
        return Err(bad());
    }
    let size = if wide { 8 } else { 4 };
    let mut at = start + 44;
    let mut transitions = Vec::with_capacity(time);
    let indices_at = at + time * size;
    for i in 0..time {
        let when = if wide { be64(data, at + i * 8) } else { be32(data, at + i * 4) }.ok_or_else(bad)?;
        let index = usize::from(*data.get(indices_at + i).ok_or_else(bad)?);
        if index >= typ {
            return Err(bad());
        }
        transitions.push((when, index));
    }
    at = indices_at + time;
    let abbrevs_at = at + typ * 6;
    let abbrevs = data.get(abbrevs_at..abbrevs_at + chars).ok_or_else(bad)?;
    let mut types = Vec::with_capacity(typ);
    for i in 0..typ {
        let offset = be32(data, at + i * 6).ok_or_else(bad)?;
        let dst = *data.get(at + i * 6 + 4).ok_or_else(bad)? != 0;
        let index = usize::from(*data.get(at + i * 6 + 5).ok_or_else(bad)?);
        let tail = abbrevs.get(index..).unwrap_or_default();
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        types.push(LocalType { offset, dst, abbrev: String::from_utf8_lossy(&tail[..end]).into_owned() });
    }
    at = abbrevs_at + chars + leap * (size + 4) + isstd + isut;
    let footer = if wide {
        match data.get(at..) {
            Some([b'\n', rest @ ..]) => {
                let end = rest.iter().position(|&b| b == b'\n').ok_or_else(bad)?;
                let text = std::str::from_utf8(&rest[..end]).map_err(|_| bad())?;
                if text.is_empty() { None } else { Some(parse_posix(text).ok_or_else(bad)?) }
            }
            _ => None,
        }
    } else {
        None
    };
    Ok(Zone { name: name.to_string(), transitions, types, footer })
}

/// Parse a POSIX TZ string such as `EST5EDT,M3.2.0,M11.1.0` or
/// `<+1030>-10:30<+11>-11,M10.1.0,M4.1.0`.
fn parse_posix(text: &str) -> Option<Rule> {
    let mut rest = text.as_bytes();
    let std_abbrev = posix_name(&mut rest)?;
    // POSIX offsets are west of Greenwich; ours are east.
    let std_offset = -posix_time(&mut rest)?;
    let std = LocalType { offset: std_offset, dst: false, abbrev: std_abbrev };
    if rest.is_empty() {
        return Some(Rule { std, dst: None });
    }
    let dst_abbrev = posix_name(&mut rest)?;
    let dst_offset = if rest.first().is_some_and(|&b| b != b',') { -posix_time(&mut rest)? } else { std_offset + 3600 };
    let dst = LocalType { offset: dst_offset, dst: true, abbrev: dst_abbrev };
    // Without rules, POSIX says to use an implementation default; the US rules are the usual one.
    if rest.is_empty() {
        return Some(Rule {
            std,
            dst: Some((dst, (RuleDay::Weekday(3, 2, 0), 7200), (RuleDay::Weekday(11, 1, 0), 7200))),
        });
    }
    rest = rest.strip_prefix(b",")?;
    let start = posix_rule(&mut rest)?;
    rest = rest.strip_prefix(b",")?;
    let end = posix_rule(&mut rest)?;
    if !rest.is_empty() {
        return None;
    }
    Some(Rule { std, dst: Some((dst, start, end)) })
}

fn posix_name(rest: &mut &[u8]) -> Option<String> {
    let (name, after) = if let Some(quoted) = rest.strip_prefix(b"<") {
        let end = quoted.iter().position(|&b| b == b'>')?;
        (&quoted[..end], &quoted[end + 1..])
    } else {
        let end = rest.iter().position(|b| !b.is_ascii_alphabetic()).unwrap_or(rest.len());
        (&rest[..end], &rest[end..])
    };
    if name.len() < 3 {
        return None;
    }
    *rest = after;
    Some(String::from_utf8_lossy(name).into_owned())
}

/// `[+-]hh[:mm[:ss]]`, in seconds.
fn posix_time(rest: &mut &[u8]) -> Option<i64> {
    let mut sign = 1;
    match rest.first() {
        Some(b'-') => {
            sign = -1;
            *rest = &rest[1..];
        }
        Some(b'+') => *rest = &rest[1..],
        _ => {}
    }
    let mut total = 0;
    for (i, scale) in [3600, 60, 1].into_iter().enumerate() {
        if i > 0 {
            match rest.strip_prefix(b":") {
                Some(after) => *rest = after,
                None => break,
            }
        }
        let end = rest.iter().position(|b| !b.is_ascii_digit()).unwrap_or(rest.len());
        if end == 0 || end > 3 {
            return None;
        }
        let value: i64 = std::str::from_utf8(&rest[..end]).ok()?.parse().ok()?;
        total += value * scale;
        *rest = &rest[end..];
    }
    Some(sign * total)
}

fn posix_number(rest: &mut &[u8]) -> Option<i64> {
    let end = rest.iter().position(|b| !b.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    let value = std::str::from_utf8(&rest[..end]).ok()?.parse().ok()?;
    *rest = &rest[end..];
    Some(value)
}

fn posix_rule(rest: &mut &[u8]) -> Option<(RuleDay, i64)> {
    let day = match rest.first()? {
        b'J' => {
            *rest = &rest[1..];
            let n = posix_number(rest)?;
            if !(1..=365).contains(&n) {
                return None;
            }
            RuleDay::Julian(n)
        }
        b'M' => {
            *rest = &rest[1..];
            let m = posix_number(rest)?;
            *rest = rest.strip_prefix(b".")?;
            let w = posix_number(rest)?;
            *rest = rest.strip_prefix(b".")?;
            let d = posix_number(rest)?;
            if !(1..=12).contains(&m) || !(1..=5).contains(&w) || !(0..=6).contains(&d) {
                return None;
            }
            RuleDay::Weekday(m, w, d)
        }
        _ => {
            let n = posix_number(rest)?;
            if n > 365 {
                return None;
            }
            RuleDay::Ordinal(n)
        }
    };
    let time = if let Some(after) = rest.strip_prefix(b"/") {
        *rest = after;
        posix_time(rest)?
    } else {
        7200
    };
    Some((day, time))
}

/// Loaded zones; id `n` is `zones[n - 1]`.
struct Zones {
    zones: Vec<Arc<Zone>>,
    by_name: HashMap<String, i64>,
    local: Option<i64>,
}

static ZONES: LazyLock<RwLock<Zones>> =
    LazyLock::new(|| RwLock::new(Zones { zones: Vec::new(), by_name: HashMap::new(), local: None }));

fn zone(id: i64) -> Option<Arc<Zone>> {
    let zones = ZONES.read().unwrap_or_else(|e| e.into_inner());
    zones.zones.get(usize::try_from(id - 1).ok()?).cloned()
}

fn zoneinfo_dir() -> std::path::PathBuf {
    match std::env::var_os("TZDIR") {
        Some(dir) if !dir.is_empty() => dir.into(),
        _ => "/usr/share/zoneinfo".into(),
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && name.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b))
}

fn read_zone(name: &str) -> Result<Zone, String> {
    if !valid_name(name) {
        return Err(format!("unknown time zone {name:?}"));
    }
    let dir = zoneinfo_dir();
    match std::fs::read(dir.join(name)) {
        Ok(data) => parse_tzif(name, &data),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if dir.is_dir() {
                Err(format!("unknown time zone {name:?}"))
            } else {
                Err(format!(
                    "unknown time zone {name:?}: there is no time zone database at {}; install one or set TZDIR",
                    dir.display()
                ))
            }
        }
        Err(e) => Err(format!("time zone {name:?}: {e}")),
    }
}

fn register(zone: Zone, key: Option<&str>) -> i64 {
    let mut zones = ZONES.write().unwrap_or_else(|e| e.into_inner());
    if let Some(key) = key
        && let Some(&id) = zones.by_name.get(key)
    {
        return id;
    }
    zones.zones.push(Arc::new(zone));
    let id = zones.zones.len() as i64;
    if let Some(key) = key {
        zones.by_name.insert(key.to_string(), id);
    }
    id
}

/// Load a zone by IANA name, such as `Europe/London`. Returns its id (1 or more), or 0 after
/// recording the error.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_tz_load(ptr: *const u8, len: i64) -> i64 {
    let name = unsafe { Str::from_raw(ptr, len) };
    if let Some(&id) = ZONES.read().unwrap_or_else(|e| e.into_inner()).by_name.get(name) {
        return id;
    }
    match read_zone(name) {
        Ok(zone) => register(zone, Some(name)),
        Err(message) => {
            record_error(message);
            0
        }
    }
}

/// The local zone, from `TZ` or `/etc/localtime`: its id, or 0 for UTC.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_tz_local() -> i64 {
    if let Some(id) = ZONES.read().unwrap_or_else(|e| e.into_inner()).local {
        return id;
    }
    let id = load_local().map_or(0, |zone| register(zone, None));
    ZONES.write().unwrap_or_else(|e| e.into_inner()).local = Some(id);
    id
}

fn load_local() -> Option<Zone> {
    match std::env::var("TZ") {
        Ok(tz) => {
            let tz = tz.strip_prefix(':').unwrap_or(&tz);
            if tz.is_empty() || tz == "UTC" {
                return None;
            }
            if tz.starts_with('/') {
                let data = std::fs::read(tz).ok()?;
                return parse_tzif(tz, &data).ok();
            }
            if let Ok(zone) = read_zone(tz) {
                return Some(zone);
            }
            let rule = parse_posix(tz)?;
            Some(Zone {
                name: tz.to_string(),
                transitions: Vec::new(),
                types: vec![rule.std.clone()],
                footer: Some(rule),
            })
        }
        Err(_) => {
            let path = std::path::Path::new("/etc/localtime");
            let data = std::fs::read(path).ok()?;
            let name = std::fs::read_link(path)
                .ok()
                .and_then(|target| {
                    let target = target.to_string_lossy().into_owned();
                    target.split_once("zoneinfo/").map(|(_, name)| name.to_string())
                })
                .unwrap_or_else(|| "Local".to_string());
            parse_tzif(&name, &data).ok()
        }
    }
}

/// The offset from UTC, in seconds, of a zone at an instant.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_tz_offset(id: i64, seconds: i64) -> i64 {
    zone(id).map_or(0, |z| z.at(seconds).offset)
}

/// The abbreviation, such as `GMT` or `BST`, of a zone at an instant.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_tz_abbrev(id: i64, seconds: i64) -> Str {
    let abbrev = zone(id).map(|z| z.at(seconds).abbrev.clone()).unwrap_or_default();
    Str::from_bytes(abbrev.as_bytes())
}

/// 1 if a zone keeps daylight saving time at an instant.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_tz_is_dst(id: i64, seconds: i64) -> i64 {
    zone(id).map_or(0, |z| i64::from(z.at(seconds).dst))
}

/// The offset to apply to a wall-clock time in a zone (seconds since the epoch as if it were
/// UTC); see `Zone::resolve`.
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_tz_resolve(id: i64, local: i64) -> i64 {
    zone(id).map_or(0, |z| z.resolve(local))
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_tz_name(id: i64) -> Str {
    let name = zone(id).map(|z| z.name.clone()).unwrap_or_else(|| "UTC".to_string());
    Str::from_bytes(name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(days_from_civil(2000, 3, 1) - days_from_civil(2000, 2, 28), 2);
        assert_eq!(days_from_civil(1900, 3, 1) - days_from_civil(1900, 2, 28), 1);
        assert_eq!(days_from_civil(2100, 3, 1) - days_from_civil(2100, 2, 28), 1);
        assert_eq!(civil(days_from_civil(9999, 12, 31)), (9999, 12, 31));
        assert_eq!(civil(days_from_civil(1, 1, 1)), (1, 1, 1));
        assert_eq!(weekday(0), 3); // a Thursday
        assert!(check_date(2023, 2, 29).is_err());
        assert!(check_date(2024, 2, 29).is_ok());
        assert!(check_date(0, 1, 1).is_err());
    }

    #[test]
    fn parsing() {
        assert_eq!(parse_datetime(b"1970-01-01T00:00:00Z"), Ok((0, 0, 0)));
        assert_eq!(parse_datetime(b"2026-11-20T09:30:00+01:00"), Ok((1_795_163_400, 0, 3600)));
        assert_eq!(parse_datetime(b"2026-11-20 08:30:00+00"), Ok((1_795_163_400, 0, 0)));
        assert_eq!(parse_datetime(b"1969-12-31T23:59:59.5Z"), Ok((-1, 500_000_000, 0)));
        assert_eq!(parse_datetime(b"2026-11-20T08:30:00.123456789-0530").map(|p| p.1), Ok(123_456_789));
        let error = parse_datetime(b"2026-11-20X08:30:00Z").unwrap_err();
        assert!(error.contains("unexpected 'X' at position 11"), "{error}");
        assert!(parse_datetime(b"2026-11-20T08:30:00").unwrap_err().contains("ends early"));
        assert!(parse_datetime(b"2026-11-20T08:30:00.1234567891Z").is_err());
        assert!(parse_datetime(b"2026-02-30T08:30:00Z").unwrap_err().contains("day 30"));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_iso(0, 0, 0), "1970-01-01T00:00:00Z");
        assert_eq!(format_iso(-1, 500_000_000, 0), "1969-12-31T23:59:59.5Z");
        assert_eq!(format_iso(1_795_163_400, 0, 3600), "2026-11-20T09:30:00+01:00");
        assert_eq!(format_iso(1_795_163_400, 0, -19800), "2026-11-20T03:00:00-05:30");
        let text = format_pattern(1_795_163_400, 250_000_000, 0, "UTC", "%a %A %b %B %j %Y-%m-%d %H:%M:%S.%f %z %Z %%");
        assert_eq!(text.unwrap(), "Fri Friday Nov November 324 2026-11-20 08:30:00.250000 +0000 UTC %");
        assert!(format_pattern(0, 0, 0, "", "%q").unwrap_err().contains("%q"));
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("1h30m"), Ok(5_400_000_000_000));
        assert_eq!(parse_duration("1.5s"), Ok(1_500_000_000));
        assert_eq!(parse_duration("-300ms"), Ok(-300_000_000));
        assert_eq!(parse_duration("1µs2ns"), Ok(1_002));
        assert_eq!(parse_duration("0"), Ok(0));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("3d").unwrap_err().contains("unknown unit `d`"));
        assert!(parse_duration("9999999999h").is_err());
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(5_400_000_000_000), "1h30m0s");
        assert_eq!(format_duration(1_500_000_000), "1.5s");
        assert_eq!(format_duration(-300_000_000), "-300ms");
        assert_eq!(format_duration(1_500), "1.5µs");
        assert_eq!(format_duration(61_000_000_000), "1m1s");
        assert_eq!(format_duration(i64::MIN), "-2562047h47m16.854775808s");
    }

    #[test]
    fn posix_rules() {
        let rule = parse_posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(rule.std.offset, -5 * 3600);
        // 2030-03-10 is the second Sunday of March: 02:00 EST is 07:00 UTC.
        let start = days_from_civil(2030, 3, 10) * DAY + 7 * 3600;
        assert_eq!(rule.at(start - 1).abbrev, "EST");
        assert_eq!(rule.at(start).abbrev, "EDT");
        // 2030-11-03 is the first Sunday of November: 02:00 EDT is 06:00 UTC.
        let end = days_from_civil(2030, 11, 3) * DAY + 6 * 3600;
        assert_eq!(rule.at(end - 1).offset, -4 * 3600);
        assert_eq!(rule.at(end).offset, -5 * 3600);
        let howe = parse_posix("<+1030>-10:30<+11>-11,M10.1.0,M4.1.0").unwrap();
        assert_eq!(howe.std.offset, 37_800);
        assert_eq!(howe.at(days_from_civil(2040, 1, 1) * DAY).offset, 39_600);
        assert_eq!(howe.at(days_from_civil(2040, 6, 1) * DAY).abbrev, "+1030");
        assert_eq!(parse_posix("<-03>3").unwrap().std.offset, -3 * 3600);
        assert!(parse_posix("X5").is_none());
    }

    fn fixture(name: &str) -> Zone {
        let path = format!("{}/../../tests/fixtures/zoneinfo/{name}", env!("CARGO_MANIFEST_DIR"));
        parse_tzif(name, &std::fs::read(path).unwrap()).unwrap()
    }

    fn utc(y: i64, m: i64, d: i64, h: i64, min: i64) -> i64 {
        days_from_civil(y, m, d) * DAY + h * 3600 + min * 60
    }

    #[test]
    fn zone_files() {
        let york = fixture("America/New_York");
        assert!(york.footer.is_some());
        // 2026-03-08 02:00 EST is 07:00 UTC: clocks go forward.
        assert_eq!(york.at(utc(2026, 3, 8, 6, 59)).abbrev, "EST");
        assert_eq!(york.at(utc(2026, 3, 8, 7, 0)).abbrev, "EDT");
        // 2026-11-01 02:00 EDT is 06:00 UTC: clocks go back.
        assert_eq!(york.at(utc(2026, 11, 1, 5, 59)).offset, -4 * 3600);
        assert_eq!(york.at(utc(2026, 11, 1, 6, 0)).offset, -5 * 3600);
        // Before the first transition: local mean time.
        assert_eq!(york.at(utc(1800, 1, 1, 0, 0)).abbrev, "LMT");
        // Past the table (2037), from the footer.
        assert_eq!(york.at(utc(2100, 7, 1, 0, 0)).abbrev, "EDT");
        assert_eq!(york.at(utc(2100, 1, 1, 0, 0)).abbrev, "EST");
        // Wall-clock times: 01:30 on 2026-11-01 happens twice (the earlier is EDT); 02:30 on
        // 2026-03-08 never happens (it becomes 03:30 EDT).
        assert_eq!(york.resolve(utc(2026, 11, 1, 1, 30)), -4 * 3600);
        assert_eq!(york.resolve(utc(2026, 3, 8, 2, 30)), -5 * 3600);
        assert_eq!(york.resolve(utc(2026, 7, 1, 12, 0)), -4 * 3600);
        assert_eq!(york.resolve(utc(2100, 11, 7, 1, 30)), -4 * 3600);

        let london = fixture("Europe/London");
        // 2026-03-29 01:00 UTC and 2026-10-25 01:00 UTC.
        assert_eq!(london.at(utc(2026, 3, 29, 0, 59)).abbrev, "GMT");
        assert_eq!(london.at(utc(2026, 3, 29, 1, 0)).abbrev, "BST");
        assert_eq!(london.at(utc(2026, 10, 25, 0, 59)).abbrev, "BST");
        assert_eq!(london.at(utc(2026, 10, 25, 1, 0)).abbrev, "GMT");
        assert_eq!(london.at(utc(2050, 6, 1, 0, 0)).offset, 3600);

        let howe = fixture("Australia/Lord_Howe");
        // 2026-04-05 02:00 +11 is 15:00 UTC on the 4th: clocks go back 30 minutes.
        assert_eq!(howe.at(utc(2026, 4, 4, 14, 59)).offset, 11 * 3600);
        assert_eq!(howe.at(utc(2026, 4, 4, 15, 0)).offset, 37_800);
        // 2026-10-04 02:00 +1030 is 15:30 UTC on the 3rd: forward 30 minutes.
        assert_eq!(howe.at(utc(2026, 10, 3, 15, 29)).offset, 37_800);
        assert_eq!(howe.at(utc(2026, 10, 3, 15, 30)).offset, 11 * 3600);
        assert_eq!(howe.resolve(utc(2026, 4, 5, 1, 45)), 11 * 3600);
        assert_eq!(howe.resolve(utc(2026, 10, 4, 2, 15)), 37_800);
        assert_eq!(howe.at(utc(2090, 1, 1, 0, 0)).abbrev, "+11");

        let utc_zone = fixture("Etc/UTC");
        assert_eq!(utc_zone.at(0).offset, 0);
        assert!(parse_tzif("bad", b"TZif2").is_err());
        assert!(valid_name("America/New_York") && !valid_name("../etc/passwd") && !valid_name("/etc/x"));
    }
}
