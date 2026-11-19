//! `lsp`: a language server over standard input and output (docs/06, milestone 11).
//!
//! It speaks the Language Server Protocol's JSON-RPC with full-document sync and offers:
//! diagnostics as you type (the compiler's own, with their help text), hover (the type of the
//! expression under the cursor), go to definition (variables, functions, methods, fields and
//! types), and formatting (the `fmt` command's layout).
//!
//! Each change re-checks the whole program the document belongs to; imported modules are read
//! from disk. That is simple, and fast enough for the programs the language has so far.

use crate::{fmt, loader};
use lang_diag::{Diagnostics, FileId, Severity, SourceMap, Span};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

// ---- a small JSON value -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> &Json {
        static NULL: Json = Json::Null;
        match self {
            Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map_or(&NULL, |(_, v)| v),
            _ => &NULL,
        }
    }

    pub fn str(&self) -> Option<&str> {
        if let Json::Str(s) = self { Some(s) } else { None }
    }

    pub fn num(&self) -> Option<f64> {
        if let Json::Num(n) = self { Some(*n) } else { None }
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => out.push_str(&format!("{}", *n as i64)),
            Json::Num(n) => out.push_str(&format!("{n}")),
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    Json::Str(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    pub fn parse(text: &str) -> Option<Json> {
        let mut parser = JsonParser { bytes: text.as_bytes(), pos: 0 };
        let value = parser.value()?;
        parser.skip_ws();
        (parser.pos == parser.bytes.len()).then_some(value)
    }
}

fn obj(fields: Vec<(&str, Json)>) -> Json {
    Json::Obj(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl JsonParser<'_> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.bytes[self.pos..].starts_with(text.as_bytes()) {
            self.pos += text.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Option<Json> {
        self.skip_ws();
        match *self.bytes.get(self.pos)? {
            b'n' => self.eat("null").then_some(Json::Null),
            b't' => self.eat("true").then_some(Json::Bool(true)),
            b'f' => self.eat("false").then_some(Json::Bool(false)),
            b'"' => self.string().map(Json::Str),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                self.skip_ws();
                if self.eat("]") {
                    return Some(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.skip_ws();
                    if self.eat(",") {
                        continue;
                    }
                    return self.eat("]").then_some(Json::Arr(items));
                }
            }
            b'{' => {
                self.pos += 1;
                let mut fields = Vec::new();
                self.skip_ws();
                if self.eat("}") {
                    return Some(Json::Obj(fields));
                }
                loop {
                    self.skip_ws();
                    let key = self.string()?;
                    self.skip_ws();
                    if !self.eat(":") {
                        return None;
                    }
                    fields.push((key, self.value()?));
                    self.skip_ws();
                    if self.eat(",") {
                        continue;
                    }
                    return self.eat("}").then_some(Json::Obj(fields));
                }
            }
            _ => {
                let start = self.pos;
                while self.pos < self.bytes.len()
                    && matches!(self.bytes[self.pos], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    self.pos += 1;
                }
                std::str::from_utf8(&self.bytes[start..self.pos]).ok()?.parse().ok().map(Json::Num)
            }
        }
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat("\"") {
            return None;
        }
        let mut out = Vec::new();
        loop {
            let b = *self.bytes.get(self.pos)?;
            self.pos += 1;
            match b {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.bytes.get(self.pos)?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'r' => out.push(b'\r'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'u' => {
                            let mut code = u32::from_str_radix(
                                std::str::from_utf8(self.bytes.get(self.pos..self.pos + 4)?).ok()?,
                                16,
                            )
                            .ok()?;
                            self.pos += 4;
                            if (0xD800..0xDC00).contains(&code) && self.eat("\\u") {
                                let low = u32::from_str_radix(
                                    std::str::from_utf8(self.bytes.get(self.pos..self.pos + 4)?).ok()?,
                                    16,
                                )
                                .ok()?;
                                self.pos += 4;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            let c = char::from_u32(code).unwrap_or('\u{FFFD}');
                            out.extend_from_slice(c.to_string().as_bytes());
                        }
                        other => out.push(other),
                    }
                }
                b => out.push(b),
            }
        }
    }
}

// ---- positions ----------------------------------------------------------------------------

/// A byte offset as an LSP position: a zero-based line, and a column in UTF-16 code units.
fn position(text: &str, offset: usize) -> Json {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let character: usize = text[line_start..offset].chars().map(char::len_utf16).sum();
    obj(vec![("line", Json::Num(line as f64)), ("character", Json::Num(character as f64))])
}

/// An LSP position as a byte offset.
fn offset(text: &str, position: &Json) -> usize {
    let line = position.get("line").num().unwrap_or(0.0) as usize;
    let character = position.get("character").num().unwrap_or(0.0) as usize;
    let mut start = 0;
    for _ in 0..line {
        match text[start..].find('\n') {
            Some(i) => start += i + 1,
            None => return text.len(),
        }
    }
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units >= character || c == '\n' {
            return start + i;
        }
        units += c.len_utf16();
    }
    text.len()
}

fn range(text: &str, span: Span) -> Json {
    obj(vec![("start", position(text, span.lo as usize)), ("end", position(text, span.hi as usize))])
}

fn path_of(uri: &str) -> Option<PathBuf> {
    let path = uri.strip_prefix("file://")?;
    let mut bytes = Vec::new();
    let raw = path.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&raw[i + 1..i + 3]).ok()?, 16) {
                bytes.push(b);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

fn uri_of(path: &Path) -> String {
    let mut uri = String::from("file://");
    for b in path.display().to_string().bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

// ---- analysis -------------------------------------------------------------------------

/// The result of checking one document's program.
struct Analysis {
    sources: SourceMap,
    diagnostics: Diagnostics,
    index: Vec<lang_sema::IndexEntry>,
}

const MAIN_FILE: FileId = FileId(0);

fn analyze(path: &Path, text: &str) -> Analysis {
    let mut sources = SourceMap::default();
    let mut diagnostics = Diagnostics::default();
    let file = sources.add(path.display().to_string(), text.to_string());
    let tokens = lang_syntax::lex(file, text, &mut diagnostics);
    let module = lang_syntax::parse(&tokens, &mut diagnostics);
    let name = path.file_stem().map_or("main".into(), |s| s.to_string_lossy().into_owned());
    let root = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let dependencies = crate::manifest::dependencies_for(path, false, false).unwrap_or_default();
    let modules = loader::load(&root, &dependencies, name, module, &mut sources, &mut diagnostics);
    let index = if diagnostics.has_errors() {
        Vec::new()
    } else {
        let inputs: Vec<lang_sema::ModuleInput> = modules
            .iter()
            .map(|m| lang_sema::ModuleInput { name: m.name.clone(), ast: &m.ast, stdlib: m.stdlib })
            .collect();
        lang_sema::analyze(&inputs, &mut sources, &mut diagnostics)
    };
    Analysis { sources, diagnostics, index }
}

impl Analysis {
    /// The document's diagnostics in LSP form. A missing `main` is not reported: the file may
    /// be a module, not a program.
    fn diagnostics(&self) -> Json {
        let text = &self.sources.get(MAIN_FILE).text;
        let items = self
            .diagnostics
            .sorted()
            .into_iter()
            .filter(|d| d.primary.span.file == MAIN_FILE && d.code != lang_diag::codes::NO_MAIN)
            .map(|d| {
                let mut message = d.message.clone();
                if !d.primary.message.is_empty() {
                    message.push_str(&format!(" ({})", d.primary.message));
                }
                if let Some(help) = &d.help {
                    message.push_str(&format!("\nhelp: {help}"));
                }
                obj(vec![
                    ("range", range(text, d.primary.span)),
                    ("severity", Json::Num(if d.severity == Severity::Error { 1.0 } else { 2.0 })),
                    ("code", Json::Str(d.code.to_string())),
                    ("source", Json::Str(lang_names::CLI_NAME.to_string())),
                    ("message", Json::Str(message)),
                ])
            })
            .collect();
        Json::Arr(items)
    }

    /// The innermost index entry covering `offset` in the document that has `what`.
    fn entry_at(&self, offset: usize, what: impl Fn(&lang_sema::IndexEntry) -> bool) -> Option<&lang_sema::IndexEntry> {
        self.index
            .iter()
            .filter(|e| e.span.file == MAIN_FILE && (e.span.lo as usize) <= offset && offset <= e.span.hi as usize)
            .filter(|e| what(e))
            .min_by_key(|e| e.span.hi - e.span.lo)
    }

    fn hover(&self, offset: usize) -> Json {
        let Some(entry) = self.entry_at(offset, |e| e.ty.is_some()) else { return Json::Null };
        let text = &self.sources.get(MAIN_FILE).text;
        let source = &text[entry.span.lo as usize..entry.span.hi as usize];
        let shown = if source.len() <= 60 && !source.contains('\n') {
            format!("{source}: {}", entry.ty.as_deref().unwrap_or(""))
        } else {
            entry.ty.clone().unwrap_or_default()
        };
        obj(vec![
            (
                "contents",
                obj(vec![("kind", Json::Str("markdown".into())), ("value", Json::Str(format!("```\n{shown}\n```")))]),
            ),
            ("range", range(text, entry.span)),
        ])
    }

    fn definition(&self, offset: usize) -> Json {
        let Some(target) = self.entry_at(offset, |e| e.definition.is_some()).and_then(|e| e.definition) else {
            return Json::Null;
        };
        let file = self.sources.get(target.file);
        let path = Path::new(&file.name);
        if !path.is_file() {
            return Json::Null; // the prelude or the standard library, compiled in
        }
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        obj(vec![("uri", Json::Str(uri_of(&path))), ("range", range(&file.text, target))])
    }
}

// ---- the server -----------------------------------------------------------------------

fn read_message(input: &mut impl BufRead) -> Option<Json> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0u8; length?];
    input.read_exact(&mut body).ok()?;
    Json::parse(std::str::from_utf8(&body).ok()?)
}

fn send(output: &mut impl Write, message: Json) {
    let text = message.to_text();
    let _ = write!(output, "Content-Length: {}\r\n\r\n{text}", text.len());
    let _ = output.flush();
}

fn reply(output: &mut impl Write, id: &Json, result: Json) {
    send(output, obj(vec![("jsonrpc", Json::Str("2.0".into())), ("id", id.clone()), ("result", result)]));
}

/// Serve until the client says `exit`. Returns the process's exit status.
pub fn serve(input: &mut impl BufRead, output: &mut impl Write) -> u8 {
    let mut documents: HashMap<String, String> = HashMap::new();
    let mut shutting_down = false;
    while let Some(message) = read_message(input) {
        let method = message.get("method").str().unwrap_or("").to_string();
        let id = message.get("id").clone();
        let params = message.get("params");
        let uri = params.get("textDocument").get("uri").str().unwrap_or("").to_string();
        match method.as_str() {
            "initialize" => {
                let capabilities = obj(vec![
                    ("textDocumentSync", Json::Num(1.0)),
                    ("hoverProvider", Json::Bool(true)),
                    ("definitionProvider", Json::Bool(true)),
                    ("documentFormattingProvider", Json::Bool(true)),
                ]);
                let info = obj(vec![("name", Json::Str(lang_names::CLI_NAME.into()))]);
                reply(output, &id, obj(vec![("capabilities", capabilities), ("serverInfo", info)]));
            }
            "shutdown" => {
                shutting_down = true;
                reply(output, &id, Json::Null);
            }
            "exit" => return if shutting_down { 0 } else { 1 },
            "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didSave" => {
                let text = match method.as_str() {
                    "textDocument/didOpen" => params.get("textDocument").get("text").str().map(String::from),
                    "textDocument/didChange" => match params.get("contentChanges") {
                        Json::Arr(changes) => changes.last().and_then(|c| c.get("text").str()).map(String::from),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(text) = text {
                    documents.insert(uri.clone(), text);
                }
                if let (Some(text), Some(path)) = (documents.get(&uri), path_of(&uri)) {
                    let analysis = analyze(&path, text);
                    let params = obj(vec![("uri", Json::Str(uri.clone())), ("diagnostics", analysis.diagnostics())]);
                    send(
                        output,
                        obj(vec![
                            ("jsonrpc", Json::Str("2.0".into())),
                            ("method", Json::Str("textDocument/publishDiagnostics".into())),
                            ("params", params),
                        ]),
                    );
                }
            }
            "textDocument/didClose" => {
                documents.remove(&uri);
            }
            "textDocument/hover" | "textDocument/definition" => {
                let result = match (documents.get(&uri), path_of(&uri)) {
                    (Some(text), Some(path)) => {
                        let analysis = analyze(&path, text);
                        let at = offset(text, params.get("position"));
                        if method == "textDocument/hover" { analysis.hover(at) } else { analysis.definition(at) }
                    }
                    _ => Json::Null,
                };
                reply(output, &id, result);
            }
            "textDocument/formatting" => {
                let result = match documents.get(&uri) {
                    Some(text) => match fmt::format(text) {
                        Some(formatted)
                            if fmt::token_fingerprint(&formatted) == fmt::token_fingerprint(text)
                                && &formatted != text =>
                        {
                            let end = position(text, text.len());
                            let whole = obj(vec![("start", position(text, 0)), ("end", end)]);
                            Json::Arr(vec![obj(vec![("range", whole), ("newText", Json::Str(formatted))])])
                        }
                        _ => Json::Arr(Vec::new()),
                    },
                    None => Json::Null,
                };
                reply(output, &id, result);
            }
            _ if !matches!(id, Json::Null) => {
                // A request this server does not handle.
                let error = obj(vec![
                    ("code", Json::Num(-32601.0)),
                    ("message", Json::Str(format!("`{method}` is not supported"))),
                ]);
                send(output, obj(vec![("jsonrpc", Json::Str("2.0".into())), ("id", id), ("error", error)]));
            }
            _ => {} // notifications it does not need
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let text = r#"{"a":[1,2.5,"x\né"],"b":null,"c":true}"#;
        let value = Json::parse(text).unwrap();
        assert_eq!(value.get("a"), &Json::Arr(vec![Json::Num(1.0), Json::Num(2.5), Json::Str("x\né".into())]));
        assert_eq!(Json::parse(&value.to_text()).unwrap(), value);
    }

    #[test]
    fn positions_count_utf16() {
        let text = "a = 1\nb = \"é😀\" + c\n";
        let at = text.find(" + c").unwrap();
        let pos = position(text, at);
        assert_eq!((pos.get("line").num(), pos.get("character").num()), (Some(1.0), Some(9.0)));
        assert_eq!(offset(text, &pos), at);
    }

    fn message(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    /// A whole session: open a document with an error, hover, go to a definition, format.
    #[test]
    fn session() {
        let dir = std::env::temp_dir().join(format!("lsp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("app.{}", lang_names::SOURCE_EXTENSION));
        let source = "def double(n: int) -> int:\n    return n*2\n\ndef main():\n    total = double(21)\n    print(total)\n    print(missing)\n";
        std::fs::write(&path, source).unwrap();
        let uri = uri_of(&path);
        let open = format!(
            r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","text":{}}}}}}}"#,
            Json::Str(source.into()).to_text()
        );
        let hover = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":5,"character":11}}}}}}"#
        );
        let definition = format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"textDocument/definition","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":4,"character":13}}}}}}"#
        );
        let format = format!(
            r#"{{"jsonrpc":"2.0","id":4,"method":"textDocument/formatting","params":{{"textDocument":{{"uri":"{uri}"}},"options":{{}}}}}}"#
        );
        let input = [
            message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#),
            message(&open),
            message(&hover),
            message(&definition),
            message(&format),
            message(r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#),
            message(r#"{"jsonrpc":"2.0","method":"exit"}"#),
        ]
        .concat();
        let mut output = Vec::new();
        let status = serve(&mut std::io::BufReader::new(input.as_bytes()), &mut output);
        assert_eq!(status, 0);
        let output = String::from_utf8(output).unwrap();
        let replies: Vec<Json> = output
            .split("Content-Length: ")
            .skip(1)
            .map(|m| Json::parse(m.split_once("\r\n\r\n").unwrap().1).unwrap())
            .collect();
        assert!(replies[0].get("result").get("capabilities").get("hoverProvider") == &Json::Bool(true));
        let diagnostics = &replies[1].get("params").get("diagnostics");
        let Json::Arr(diagnostics) = diagnostics else { panic!("{diagnostics:?}") };
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].get("message").str().unwrap().contains("missing"));
        assert_eq!(diagnostics[0].get("range").get("start").get("line").num(), Some(6.0));
        let hover = replies[2].get("result").get("contents").get("value").str().unwrap();
        assert!(hover.contains("total: int"), "{hover}");
        let target = replies[3].get("result");
        assert_eq!(target.get("uri").str(), Some(uri.as_str()));
        assert_eq!(target.get("range").get("start").get("line").num(), Some(0.0));
        let Json::Arr(edits) = replies[4].get("result") else { panic!() };
        assert!(edits[0].get("newText").str().unwrap().contains("return n * 2"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
