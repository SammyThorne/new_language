//! Python-compatible text formatting of numbers and strings.
//!
//! - [`float_repr`] is how a `float` prints: the shortest text that reads back as the same
//!   number, with Python's choice between fixed and scientific notation.
//! - [`Spec`] is Python's format-specification mini-language, used after `:` in f-strings:
//!   `[[fill]align][sign][#][0][width][grouping][.precision][type]`.
//!
//! The compiler uses [`Spec::parse`] and [`Spec::check`] to reject bad specs at compile time; the
//! runtime uses the `format_*` functions to apply them.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
    /// Padding goes between the sign and the digits (`=`).
    AfterSign,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    /// Only negative numbers get a sign (the default).
    Minus,
    Plus,
    Space,
}

/// What kind of value a spec is applied to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Int,
    Float,
    Str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub fill: char,
    pub align: Option<Align>,
    pub sign: Sign,
    pub alternate: bool,
    pub zero: bool,
    pub width: usize,
    pub grouping: Option<char>,
    pub precision: Option<usize>,
    pub kind: Option<char>,
}

impl Spec {
    pub fn parse(spec: &str) -> Result<Spec, String> {
        let chars: Vec<char> = spec.chars().collect();
        let mut i = 0;
        let mut out = Spec {
            fill: ' ',
            align: None,
            sign: Sign::Minus,
            alternate: false,
            zero: false,
            width: 0,
            grouping: None,
            precision: None,
            kind: None,
        };
        let align_of = |c: char| match c {
            '<' => Some(Align::Left),
            '>' => Some(Align::Right),
            '^' => Some(Align::Center),
            '=' => Some(Align::AfterSign),
            _ => None,
        };
        if chars.len() >= 2
            && let Some(align) = align_of(chars[1])
        {
            out.fill = chars[0];
            out.align = Some(align);
            i = 2;
        } else if let Some(align) = chars.first().copied().and_then(align_of) {
            out.align = Some(align);
            i = 1;
        }
        match chars.get(i) {
            Some('+') => (out.sign, i) = (Sign::Plus, i + 1),
            Some('-') => (out.sign, i) = (Sign::Minus, i + 1),
            Some(' ') => (out.sign, i) = (Sign::Space, i + 1),
            _ => {}
        }
        if chars.get(i) == Some(&'z') {
            return Err("the `z` option is not supported".into());
        }
        if chars.get(i) == Some(&'#') {
            out.alternate = true;
            i += 1;
        }
        if chars.get(i) == Some(&'0') {
            out.zero = true;
            i += 1;
        }
        let start = i;
        while chars.get(i).is_some_and(char::is_ascii_digit) {
            i += 1;
        }
        if i > start {
            let digits: String = chars[start..i].iter().collect();
            out.width = digits.parse().map_err(|_| "width is too large".to_string())?;
        }
        if let Some(&c @ (',' | '_')) = chars.get(i) {
            out.grouping = Some(c);
            i += 1;
        }
        if chars.get(i) == Some(&'.') {
            i += 1;
            let start = i;
            while chars.get(i).is_some_and(char::is_ascii_digit) {
                i += 1;
            }
            if i == start {
                return Err("expected digits after `.` for the precision".into());
            }
            let digits: String = chars[start..i].iter().collect();
            out.precision = Some(digits.parse().map_err(|_| "precision is too large".to_string())?);
        }
        if let Some(&c) = chars.get(i) {
            if !"bdeEfFgGosxX%".contains(c) {
                return Err(format!("unknown format type `{c}`"));
            }
            out.kind = Some(c);
            i += 1;
        }
        if i != chars.len() {
            return Err(format!("unexpected `{}` in format spec", chars[i..].iter().collect::<String>()));
        }
        Ok(out)
    }

    /// Whether this spec can be applied to a value of the given kind.
    pub fn check(&self, kind: Kind) -> Result<(), String> {
        let ty = self.kind;
        let float_type = matches!(ty, Some('e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%'));
        match kind {
            Kind::Int => {
                if !matches!(ty, None | Some('b' | 'd' | 'o' | 'x' | 'X')) && !float_type {
                    return Err(format!("format type `{}` cannot be used with `int`", ty.unwrap()));
                }
                if self.precision.is_some() && !float_type {
                    return Err("precision is not allowed for `int` unless the type is a float type such as `f`".into());
                }
            }
            Kind::Float => {
                if let Some(c) = ty
                    && !float_type
                {
                    return Err(format!("format type `{c}` cannot be used with `float`"));
                }
            }
            Kind::Str => {
                if !matches!(ty, None | Some('s')) {
                    return Err(format!("format type `{}` cannot be used with `str`", ty.unwrap()));
                }
                if self.sign != Sign::Minus {
                    return Err("a sign is not allowed for `str`".into());
                }
                if self.grouping.is_some() {
                    return Err("grouping is not allowed for `str`".into());
                }
                if self.align == Some(Align::AfterSign) {
                    return Err("`=` alignment is not allowed for `str`".into());
                }
            }
        }
        if self.alternate && !matches!(ty, Some('b' | 'o' | 'x' | 'X')) {
            return Err("`#` is only supported with the `b`, `o`, `x` and `X` types".into());
        }
        Ok(())
    }
}

/// How a `float` prints, matching Python's `repr`.
pub fn float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-inf".into() } else { "inf".into() };
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x == 0.0 {
        return format!("{sign}0.0");
    }
    // Rust's `{:e}` gives the shortest digits that round-trip, e.g. `1.5e-5`.
    let (digits, exp) = shortest_digits(x.abs());
    if (-4..16).contains(&exp) {
        format!("{sign}{}", fixed_from_digits(&digits, exp, true))
    } else {
        let mantissa = if digits.len() == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{sign}{mantissa}e{}", exponent_text(exp))
    }
}

/// Shortest round-trip decimal digits (no dot) and the decimal exponent of the first digit.
fn shortest_digits(x: f64) -> (String, i32) {
    let text = format!("{x:e}");
    let (mantissa, exp) = text.split_once('e').unwrap();
    (mantissa.replace('.', ""), exp.parse().unwrap())
}

/// Place a decimal point in `digits` (first digit has exponent `exp`).
fn fixed_from_digits(digits: &str, exp: i32, force_point: bool) -> String {
    let n = digits.len() as i32;
    let text = if exp < 0 {
        format!("0.{}{}", "0".repeat((-exp - 1) as usize), digits)
    } else if exp + 1 >= n {
        format!("{}{}", digits, "0".repeat((exp + 1 - n) as usize))
    } else {
        format!("{}.{}", &digits[..(exp + 1) as usize], &digits[(exp + 1) as usize..])
    };
    if force_point && !text.contains('.') { format!("{text}.0") } else { text }
}

fn exponent_text(exp: i32) -> String {
    format!("{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
}

/// Rust's `{:.Ne}` output (`1.50e3`) in Python's style (`1.50e+03`).
fn scientific(x: f64, precision: usize) -> (String, i32) {
    let text = format!("{x:.precision$e}");
    let (mantissa, exp) = text.split_once('e').unwrap();
    (mantissa.to_string(), exp.parse().unwrap())
}

fn group_digits(digits: &str, separator: char, every: usize) -> String {
    let chars: Vec<char> = digits.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % every == 0 {
            out.push(separator);
        }
        out.push(*c);
    }
    out
}

/// Apply width, fill and alignment. `sign` is the sign or prefix that `=` alignment pads after.
fn pad(spec: &Spec, sign: &str, body: &str, default: Align) -> String {
    let (fill, align) = match (spec.align, spec.zero) {
        (Some(align), _) => (spec.fill, align),
        (None, true) => ('0', Align::AfterSign),
        (None, false) => (spec.fill, default),
    };
    let len = sign.chars().count() + body.chars().count();
    if spec.width <= len {
        return format!("{sign}{body}");
    }
    let n = spec.width - len;
    let fill_text = |count: usize| fill.to_string().repeat(count);
    match align {
        Align::Left => format!("{sign}{body}{}", fill_text(n)),
        Align::Right => format!("{}{sign}{body}", fill_text(n)),
        Align::Center => format!("{}{sign}{body}{}", fill_text(n / 2), fill_text(n - n / 2)),
        Align::AfterSign => format!("{sign}{}{body}", fill_text(n)),
    }
}

fn sign_text(spec: &Spec, negative: bool) -> &'static str {
    match (negative, spec.sign) {
        (true, _) => "-",
        (false, Sign::Plus) => "+",
        (false, Sign::Space) => " ",
        (false, Sign::Minus) => "",
    }
}

pub fn format_int(value: i64, spec: &Spec) -> String {
    if matches!(spec.kind, Some('e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%')) {
        return format_float(value as f64, spec);
    }
    let magnitude = value.unsigned_abs();
    let (digits, prefix, group_every) = match spec.kind {
        Some('b') => (format!("{magnitude:b}"), "0b", 4),
        Some('o') => (format!("{magnitude:o}"), "0o", 4),
        Some('x') => (format!("{magnitude:x}"), "0x", 4),
        Some('X') => (format!("{magnitude:X}"), "0X", 4),
        _ => (magnitude.to_string(), "", 3),
    };
    let digits = match spec.grouping {
        Some(separator) => group_digits(&digits, separator, group_every),
        None => digits,
    };
    let mut sign = sign_text(spec, value < 0).to_string();
    if spec.alternate {
        sign.push_str(prefix);
    }
    pad(spec, &sign, &digits, Align::Right)
}

pub fn format_float(value: f64, spec: &Spec) -> String {
    let negative = value.is_sign_negative() && !value.is_nan();
    let x = value.abs();
    let upper = matches!(spec.kind, Some('E' | 'F' | 'G'));
    let body = if !x.is_finite() {
        let text = if x.is_nan() { "nan" } else { "inf" };
        let text = if upper { text.to_uppercase() } else { text.to_string() };
        if spec.kind == Some('%') { format!("{text}%") } else { text }
    } else {
        match (spec.kind, spec.precision) {
            (None, None) => float_repr(x),
            (Some('f' | 'F'), p) => format!("{x:.*}", p.unwrap_or(6)),
            (Some('%'), p) => format!("{:.*}%", p.unwrap_or(6), x * 100.0),
            (Some('e' | 'E'), p) => {
                let (mantissa, exp) = scientific(x, p.unwrap_or(6));
                format!("{mantissa}e{}", exponent_text(exp))
            }
            (Some('g' | 'G'), p) => general(x, p.unwrap_or(6), spec.alternate, false),
            (None, Some(p)) => general(x, p, spec.alternate, true),
            (Some(other), _) => unreachable!("format type `{other}` was rejected by check"),
        }
    };
    let body = if upper { body.to_uppercase() } else { body };
    let body = match spec.grouping {
        Some(separator) => {
            let split = body.find(|c: char| !c.is_ascii_digit()).unwrap_or(body.len());
            format!("{}{}", group_digits(&body[..split], separator, 3), &body[split..])
        }
        None => body,
    };
    pad(spec, sign_text(spec, negative), &body, Align::Right)
}

/// Python's `g` presentation. `no_type` is the variant used when only a precision is given,
/// which keeps a `.0` and switches to scientific notation one digit earlier.
fn general(x: f64, precision: usize, alternate: bool, no_type: bool) -> String {
    let p = precision.max(1) as i32;
    let exp = if x == 0.0 { 0 } else { scientific(x, (p - 1) as usize).1 };
    let fixed_limit = if no_type { p - 1 } else { p };
    if -4 <= exp && exp < fixed_limit {
        let decimals = (p - 1 - exp).max(0) as usize;
        let mut text = format!("{x:.decimals$}");
        if !alternate && text.contains('.') {
            text = text.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        if no_type && !text.contains('.') {
            text.push_str(".0");
        }
        text
    } else {
        let (mut mantissa, exp) = scientific(x, (p - 1) as usize);
        if !alternate && mantissa.contains('.') {
            mantissa = mantissa.trim_end_matches('0').trim_end_matches('.').to_string();
        }
        format!("{mantissa}e{}", exponent_text(exp))
    }
}

pub fn format_str(value: &str, spec: &Spec) -> String {
    let value: String = match spec.precision {
        Some(p) => value.chars().take(p).collect(),
        None => value.to_string(),
    };
    pad(spec, "", &value, Align::Left)
}

#[cfg(test)]
// The test values are what Python prints for them, not approximations of π.
#[allow(clippy::approx_constant)]
mod tests {
    use super::*;

    fn int(v: i64, spec: &str) -> String {
        let spec = Spec::parse(spec).unwrap();
        spec.check(Kind::Int).unwrap();
        format_int(v, &spec)
    }

    fn float(v: f64, spec: &str) -> String {
        let spec = Spec::parse(spec).unwrap();
        spec.check(Kind::Float).unwrap();
        format_float(v, &spec)
    }

    fn string(v: &str, spec: &str) -> String {
        let spec = Spec::parse(spec).unwrap();
        spec.check(Kind::Str).unwrap();
        format_str(v, &spec)
    }

    /// Expected values are what CPython 3.13 prints.
    #[test]
    fn repr_matches_python() {
        for (value, expected) in [
            (0.1, "0.1"),
            (1.0, "1.0"),
            (3.14, "3.14"),
            (-0.0, "-0.0"),
            (0.0, "0.0"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (123456789012345678.0, "1.2345678901234568e+17"),
            (1.5e-5, "1.5e-05"),
            (0.0001, "0.0001"),
            (1e22, "1e+22"),
            (2.5, "2.5"),
            (1.0 / 3.0, "0.3333333333333333"),
            (100.0, "100.0"),
            (f64::NAN, "nan"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (5e-324, "5e-324"),
        ] {
            assert_eq!(float_repr(value), expected, "repr({value:?})");
        }
    }

    #[test]
    fn ints_match_python() {
        assert_eq!(int(42, "05d"), "00042");
        assert_eq!(int(-42, "05d"), "-0042");
        assert_eq!(int(255, "x"), "ff");
        assert_eq!(int(255, "#x"), "0xff");
        assert_eq!(int(255, "#010b"), "0b11111111");
        assert_eq!(int(1234567, ","), "1,234,567");
        assert_eq!(int(1234567, "_"), "1_234_567");
        assert_eq!(int(42, ">6"), "    42");
        assert_eq!(int(42, "<6"), "42    ");
        assert_eq!(int(42, "^6"), "  42  ");
        assert_eq!(int(42, "*^7"), "**42***");
        assert_eq!(int(5, "+"), "+5");
        assert_eq!(int(5, " d"), " 5");
        assert_eq!(int(-5, "=6"), "-    5");
        assert_eq!(int(3, ".2f"), "3.00");
        assert_eq!(int(i64::MIN, ""), "-9223372036854775808");
    }

    #[test]
    fn floats_match_python() {
        assert_eq!(float(3.14159, ".2f"), "3.14");
        assert_eq!(float(1234567.891, ",.2f"), "1,234,567.89");
        assert_eq!(float(2.5, ".0f"), "2");
        assert_eq!(float(0.25, "%"), "25.000000%");
        assert_eq!(float(0.25, ".1%"), "25.0%");
        assert_eq!(float(12345.678, "e"), "1.234568e+04");
        assert_eq!(float(12345.678, ".3g"), "1.23e+04");
        assert_eq!(float(0.0001234, "g"), "0.0001234");
        assert_eq!(float(123.0, "g"), "123");
        assert_eq!(float(1.0, ".3"), "1.0");
        assert_eq!(float(100.0, ".3"), "1e+02");
        assert_eq!(float(3.14159, ".3"), "3.14");
        assert_eq!(float(3.0, ""), "3.0");
        assert_eq!(float(1e-5, "f"), "0.000010");
        assert_eq!(float(-1.5, "+.1f"), "-1.5");
        assert_eq!(float(1.5, "+.1f"), "+1.5");
        assert_eq!(float(3.14159, "8.3f"), "   3.142");
        assert_eq!(float(-3.14159, "08.3f"), "-003.142");
        assert_eq!(float(f64::NAN, "f"), "nan");
        assert_eq!(float(f64::INFINITY, "F"), "INF");
        assert_eq!(float(0.0, "g"), "0");
    }

    #[test]
    fn strings_match_python() {
        assert_eq!(string("ab", "*^6"), "**ab**");
        assert_eq!(string("ab", "5"), "ab   ");
        assert_eq!(string("ab", ">5"), "   ab");
        assert_eq!(string("abcdef", ".3"), "abc");
    }

    #[test]
    fn bad_specs_are_rejected() {
        assert!(Spec::parse("q").is_err());
        assert!(Spec::parse(".f").is_err());
        assert!(Spec::parse("5x5").is_err());
        assert!(Spec::parse("d").unwrap().check(Kind::Float).is_err());
        assert!(Spec::parse(".2").unwrap().check(Kind::Int).is_err());
        assert!(Spec::parse("+").unwrap().check(Kind::Str).is_err());
        assert!(Spec::parse("f").unwrap().check(Kind::Str).is_err());
    }
}
