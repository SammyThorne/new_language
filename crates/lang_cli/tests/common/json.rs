//! Just enough JSON, JSON Schema and YAML for the OpenAPI tests (docs/12): no packages can be
//! downloaded, so these are written here. The JSON reader is complete; the schema validator
//! covers the keywords the compiler generates; the YAML reader covers what `http.lang` writes
//! (block mappings and sequences, plain and double-quoted scalars, `[]` and `{}`).

use std::fmt::Write;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// A number, and whether it was written as an integer.
    Num(f64, bool),
    Str(String),
    Arr(Vec<Json>),
    /// Keys in the order written.
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Follow keys: `doc.at(&["paths", "/todos", "get"])`.
    pub fn at(&self, path: &[&str]) -> &Json {
        let mut value = self;
        for key in path {
            value = value.get(key).unwrap_or_else(|| panic!("no {key:?} in {path:?}"));
        }
        value
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Json::Str(text) = self { Some(text) } else { None }
    }

    pub fn keys(&self) -> Vec<&str> {
        match self {
            Json::Obj(pairs) => pairs.iter().map(|(k, _)| k.as_str()).collect(),
            _ => Vec::new(),
        }
    }

    /// Indented with two spaces, for golden files.
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out.push('\n');
        out
    }

    fn write_pretty(&self, out: &mut String, depth: usize) {
        let pad = |depth: usize| "  ".repeat(depth);
        match self {
            Json::Arr(items) if !items.is_empty() => {
                out.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    item.write_pretty(out, depth + 1);
                    out.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Json::Obj(pairs) if !pairs.is_empty() => {
                out.push_str("{\n");
                for (index, (key, value)) in pairs.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    write_string(key, out);
                    out.push_str(": ");
                    value.write_pretty(out, depth + 1);
                    out.push_str(if index + 1 < pairs.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            Json::Arr(_) => out.push_str("[]"),
            Json::Obj(_) => out.push_str("{}"),
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(v, true) => write!(out, "{}", *v as i64).unwrap(),
            Json::Num(v, false) => write!(out, "{v:?}").unwrap(),
            Json::Str(text) => write_string(text, out),
        }
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Parse a JSON text; the error says where it went wrong.
pub fn parse(text: &str) -> Result<Json, String> {
    let mut reader = Reader { chars: text.chars().collect(), at: 0 };
    let value = reader.value()?;
    reader.space();
    if reader.at != reader.chars.len() {
        return Err(format!("text after the value at {}", reader.at));
    }
    Ok(value)
}

struct Reader {
    chars: Vec<char>,
    at: usize,
}

impl Reader {
    fn space(&mut self) {
        while self.chars.get(self.at).is_some_and(|c| c.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        self.space();
        if self.peek() == Some(c) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected {c:?} at {}, found {:?}", self.at, self.peek()))
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Result<Json, String> {
        let end = self.at + word.len();
        if end <= self.chars.len() && self.chars[self.at..end].iter().collect::<String>() == word {
            self.at = end;
            Ok(value)
        } else {
            Err(format!("bad literal at {}", self.at))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.space();
        match self.peek() {
            Some('{') => {
                self.at += 1;
                let mut pairs = Vec::new();
                self.space();
                if self.peek() == Some('}') {
                    self.at += 1;
                    return Ok(Json::Obj(pairs));
                }
                loop {
                    self.space();
                    let key = self.string()?;
                    self.expect(':')?;
                    pairs.push((key, self.value()?));
                    self.space();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some('}') => {
                            self.at += 1;
                            return Ok(Json::Obj(pairs));
                        }
                        other => return Err(format!("expected , or }} at {}, found {other:?}", self.at)),
                    }
                }
            }
            Some('[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.peek() == Some(']') {
                    self.at += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    match self.peek() {
                        Some(',') => self.at += 1,
                        Some(']') => {
                            self.at += 1;
                            return Ok(Json::Arr(items));
                        }
                        other => return Err(format!("expected , or ] at {}, found {other:?}", self.at)),
                    }
                }
            }
            Some('"') => Ok(Json::Str(self.string()?)),
            Some('t') => self.word("true", Json::Bool(true)),
            Some('f') => self.word("false", Json::Bool(false)),
            Some('n') => self.word("null", Json::Null),
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let start = self.at;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || "+-.eE".contains(c)) {
                    self.at += 1;
                }
                let text: String = self.chars[start..self.at].iter().collect();
                let value = text.parse::<f64>().map_err(|_| format!("bad number {text:?}"))?;
                Ok(Json::Num(value, !text.contains(['.', 'e', 'E'])))
            }
            other => Err(format!("unexpected {other:?} at {}", self.at)),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some('"') {
            return Err(format!("expected a string at {}", self.at));
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unterminated string")?;
            self.at += 1;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.peek().ok_or("unterminated escape")?;
                    self.at += 1;
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        '/' => out.push('/'),
                        '\\' => out.push('\\'),
                        '"' => out.push('"'),
                        'u' => {
                            let hex: String = self.chars.get(self.at..self.at + 4).ok_or("short \\u")?.iter().collect();
                            self.at += 4;
                            let mut code = u32::from_str_radix(&hex, 16).map_err(|_| "bad \\u")?;
                            if (0xD800..0xDC00).contains(&code) {
                                let low: String =
                                    self.chars.get(self.at + 2..self.at + 6).ok_or("short surrogate")?.iter().collect();
                                self.at += 6;
                                let low = u32::from_str_radix(&low, 16).map_err(|_| "bad surrogate")?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            out.push(char::from_u32(code).ok_or("bad code point")?);
                        }
                        other => return Err(format!("bad escape \\{other}")),
                    }
                }
                c if (c as u32) < 0x20 => return Err("control character in a string".into()),
                c => out.push(c),
            }
        }
    }
}

/// Check `value` against `schema`, resolving `$ref`s in `doc`. Returns what does not fit.
pub fn validate(doc: &Json, schema: &Json, value: &Json) -> Result<(), String> {
    check(doc, schema, value, "$", 0)
}

fn check(doc: &Json, schema: &Json, value: &Json, at: &str, depth: usize) -> Result<(), String> {
    if depth > 64 {
        return Err(format!("{at}: schema nests too deeply"));
    }
    let fail = |why: String| Err(format!("{at}: {why} (value {value:?})"));
    match schema {
        Json::Bool(true) => return Ok(()),
        Json::Bool(false) => return fail("no value is allowed here".into()),
        Json::Obj(_) => {}
        other => return fail(format!("bad schema {other:?}")),
    }
    if let Some(Json::Str(target)) = schema.get("$ref") {
        let name = target.strip_prefix("#/components/schemas/").ok_or(format!("unsupported $ref {target}"))?;
        let resolved = doc.at(&["components", "schemas"]).get(name).ok_or(format!("{at}: no schema {name}"))?;
        check(doc, resolved, value, at, depth + 1)?;
    }
    if let Some(types) = schema.get("type") {
        let names: Vec<&str> = match types {
            Json::Str(name) => vec![name.as_str()],
            Json::Arr(items) => items.iter().filter_map(Json::as_str).collect(),
            other => return fail(format!("bad type {other:?}")),
        };
        let fits = names.iter().any(|name| match (*name, value) {
            ("null", Json::Null) | ("boolean", Json::Bool(_)) | ("string", Json::Str(_)) => true,
            ("number", Json::Num(..)) | ("array", Json::Arr(_)) | ("object", Json::Obj(_)) => true,
            ("integer", Json::Num(v, _)) => v.fract() == 0.0,
            _ => false,
        });
        if !fits {
            return fail(format!("not of type {names:?}"));
        }
    }
    if let Some(Json::Str(format)) = schema.get("format")
        && let Json::Num(v, _) = value
        && format == "int32"
        && !(-2147483648.0..=2147483647.0).contains(v)
    {
        return fail("out of the int32 range".into());
    }
    if let (Some(Json::Num(minimum, _)), Json::Num(v, _)) = (schema.get("minimum"), value)
        && v < minimum
    {
        return fail(format!("below the minimum {minimum}"));
    }
    if let Some(Json::Arr(options)) = schema.get("enum")
        && !options.contains(value)
    {
        return fail("not one of the enum's values".into());
    }
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        return fail(format!("not the constant {expected:?}"));
    }
    if let Some(Json::Arr(branches)) = schema.get("oneOf") {
        let matching = branches.iter().filter(|b| check(doc, b, value, at, depth + 1).is_ok()).count();
        if matching != 1 {
            return fail(format!("fits {matching} of the oneOf branches, not exactly one"));
        }
    }
    if let Some(Json::Arr(all)) = schema.get("allOf") {
        for branch in all {
            check(doc, branch, value, at, depth + 1)?;
        }
    }
    if let Json::Arr(items) = value {
        let prefix = match schema.get("prefixItems") {
            Some(Json::Arr(prefix)) => prefix.as_slice(),
            _ => &[],
        };
        for (index, item) in items.iter().enumerate() {
            let item_schema = match prefix.get(index) {
                Some(s) => Some(s),
                None => schema.get("items"),
            };
            if let Some(item_schema) = item_schema {
                check(doc, item_schema, item, &format!("{at}[{index}]"), depth + 1)?;
            }
        }
        let len = items.len() as f64;
        if let Some(Json::Num(min, _)) = schema.get("minItems")
            && len < *min
        {
            return fail(format!("fewer than {min} items"));
        }
        if let Some(Json::Num(max, _)) = schema.get("maxItems")
            && len > *max
        {
            return fail(format!("more than {max} items"));
        }
        if schema.get("uniqueItems") == Some(&Json::Bool(true))
            && items.iter().enumerate().any(|(i, a)| items[..i].contains(a))
        {
            return fail("items repeat".into());
        }
    }
    if let Json::Obj(pairs) = value {
        if let Some(Json::Arr(required)) = schema.get("required") {
            for name in required.iter().filter_map(Json::as_str) {
                if value.get(name).is_none() {
                    return fail(format!("missing required {name:?}"));
                }
            }
        }
        let properties = schema.get("properties");
        for (key, item) in pairs {
            match properties.and_then(|p| p.get(key)) {
                Some(property) => check(doc, property, item, &format!("{at}.{key}"), depth + 1)?,
                None => {
                    if let Some(extra) = schema.get("additionalProperties") {
                        check(doc, extra, item, &format!("{at}.{key}"), depth + 1)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Read the YAML that `http.lang` writes for the OpenAPI document.
pub fn parse_yaml(text: &str) -> Result<Json, String> {
    let lines: Vec<(usize, &str)> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let content = line.trim_start_matches(' ');
            (line.len() - content.len(), content)
        })
        .collect();
    let mut at = 0;
    let value = yaml_block(&lines, &mut at, 0)?;
    if at != lines.len() {
        return Err(format!("unexpected line {:?}", lines[at].1));
    }
    Ok(value)
}

fn yaml_block(lines: &[(usize, &str)], at: &mut usize, indent: usize) -> Result<Json, String> {
    let (_, first) = lines.get(*at).ok_or("a block is missing")?;
    if *first == "-" || first.starts_with("- ") {
        let mut items = Vec::new();
        while let Some((level, line)) = lines.get(*at)
            && *level == indent
            && (*line == "-" || line.starts_with("- "))
        {
            *at += 1;
            items.push(match line.strip_prefix("- ") {
                Some(scalar) => yaml_scalar(scalar)?,
                None => yaml_block(lines, at, indent + 2)?,
            });
        }
        return Ok(Json::Arr(items));
    }
    let mut pairs = Vec::new();
    while let Some((level, line)) = lines.get(*at)
        && *level == indent
    {
        *at += 1;
        let (key, rest) = if line.starts_with('"') {
            let mut reader = Reader { chars: line.chars().collect(), at: 0 };
            let key = reader.string()?;
            let rest: String = reader.chars[reader.at..].iter().collect();
            (key, rest)
        } else {
            let colon = line.find(':').ok_or(format!("no `:` in {line:?}"))?;
            (line[..colon].to_string(), line[colon..].to_string())
        };
        let value = match rest.as_str() {
            ":" => yaml_block(lines, at, indent + 2)?,
            _ => yaml_scalar(rest.strip_prefix(": ").ok_or(format!("bad mapping line {line:?}"))?)?,
        };
        pairs.push((key, value));
    }
    Ok(Json::Obj(pairs))
}

fn yaml_scalar(text: &str) -> Result<Json, String> {
    Ok(match text {
        "null" => Json::Null,
        "true" => Json::Bool(true),
        "false" => Json::Bool(false),
        "[]" => Json::Arr(Vec::new()),
        "{}" => Json::Obj(Vec::new()),
        _ if text.starts_with('"') => parse(text)?,
        _ if text.starts_with(|c: char| c == '-' || c.is_ascii_digit()) => parse(text)?,
        _ => Json::Str(text.to_string()),
    })
}
