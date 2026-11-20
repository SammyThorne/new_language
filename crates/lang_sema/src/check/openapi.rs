//! The OpenAPI description of annotated routes (docs/12-openapi-docs-plan.md).
//!
//! Everything here happens at compile time. For each route that `server.mount` registers, the
//! checker writes the OpenAPI operation as JSON text, and for each `@derive(Json)` type the
//! operations reach, the JSON Schema of its wire form, following the encoder and decoder in
//! `lang_codegen/src/json.rs`. The generated adapter source hands both to the server
//! (`__server.__document(...)`, `__server.__schema(...)`), which only joins text at run time.
//!
//! None of it is generated unless the program calls `docs` or `openapi_json` somewhere, so a
//! server that does not document itself is compiled exactly as before.

use super::http::{Binding, Reply, Route, Scalar};
use super::{Checker, ModuleInput};
use crate::hir::{self, ExprKind as K};
use crate::types::{SizedKind, Ty, TyKind};
use lang_diag::{Diagnostic, Diagnostics, Span, codes};
use lang_syntax::token::TokenKind;

/// A JSON value being built; written out with [`J::write`].
#[derive(Clone, Debug, PartialEq)]
pub(super) enum J {
    Null,
    Bool(bool),
    Int(i64),
    /// A number, already formatted.
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    fn obj(pairs: Vec<(&str, J)>) -> J {
        J::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn s(text: &str) -> J {
        J::Str(text.to_string())
    }

    pub(super) fn text(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            J::Null => out.push_str("null"),
            J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            J::Int(v) => out.push_str(&v.to_string()),
            J::Num(text) => out.push_str(text),
            J::Str(text) => write_str(text, out),
            J::Arr(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            J::Obj(pairs) => {
                out.push('{');
                for (index, (key, value)) in pairs.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_str(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// A JSON string literal. Control characters are escaped, so the text has no raw line breaks.
fn write_str(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A string literal in generated source holding `text`, which has no control characters.
pub(super) fn source_literal(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The schema of the body of an error response, `{"error": "..."}` (docs/05, "Error mapping").
pub(super) const ERROR_SCHEMA: &str = "Error";

/// The standard reason phrase of a status, used as a response's description.
pub(super) fn reason(status: i64) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        203 => "Non-Authoritative Information",
        204 => "No Content",
        205 => "Reset Content",
        206 => "Partial Content",
        300 => "Multiple Choices",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        406 => "Not Acceptable",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        416 => "Range Not Satisfiable",
        417 => "Expectation Failed",
        418 => "I'm a teapot",
        421 => "Misdirected Request",
        422 => "Unprocessable Content",
        423 => "Locked",
        424 => "Failed Dependency",
        425 => "Too Early",
        426 => "Upgrade Required",
        428 => "Precondition Required",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        451 => "Unavailable For Legal Reasons",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        505 => "HTTP Version Not Supported",
        507 => "Insufficient Storage",
        508 => "Loop Detected",
        511 => "Network Authentication Required",
        _ if status < 200 => "Informational",
        _ if status < 300 => "Success",
        _ if status < 400 => "Redirection",
        _ if status < 500 => "Client Error",
        _ => "Server Error",
    }
}

/// Whether the program calls a method named `docs` or `openapi_json` anywhere in its own files:
/// only then does `mount` generate the documentation. Looking at tokens can only say yes too
/// often (another type's `docs` method), never too rarely.
pub(super) fn program_uses_docs(inputs: &[ModuleInput], sources: &lang_diag::SourceMap) -> bool {
    inputs.iter().filter(|m| !m.stdlib).any(|input| {
        let file = input.ast.start.file;
        let mut ignored = Diagnostics::default();
        let tokens = lang_syntax::lex(file, &sources.get(file).text, &mut ignored);
        tokens.windows(2).any(|pair| {
            pair[0].kind == TokenKind::Dot
                && matches!(&pair[1].kind, TokenKind::Name(name) if name == "docs" || name == "openapi_json")
        })
    })
}

/// What one `mount` documents: the operations, in route order, and the schemas they reach.
pub(super) struct Documented {
    /// For each route: its `operationId`, the rest of the operation as JSON text, and whether
    /// it is a WebSocket endpoint. The server puts the id in, made unique across mounts.
    pub operations: Vec<(String, String, bool)>,
    /// Component name and schema as JSON text.
    pub schemas: Vec<(String, String)>,
}

impl Checker<'_, '_> {
    /// The operations of `routes` and every schema they need. `owner` is the mounted value's
    /// type (`None` for a function). Reports a name shared by two types at `span`.
    pub(super) fn document_routes(&mut self, routes: &[Route], owner: Option<Ty>, span: Span) -> Documented {
        let mut pending: Vec<Ty> = Vec::new();
        let mut needs_error = false;
        let mut operations = Vec::new();
        for route in routes {
            let (id, operation, error) = self.operation(route, owner, &mut pending);
            needs_error |= error;
            operations.push((id, operation.text(), route.ws));
        }
        let mut schemas = Vec::new();
        let mut done: Vec<Ty> = Vec::new();
        while let Some(ty) = pending.pop() {
            if done.contains(&ty) {
                continue;
            }
            done.push(ty);
            let name = self.component_name(ty, Some(span));
            let schema = self.component(ty, &mut pending);
            schemas.push((name, schema.text()));
        }
        if needs_error {
            let schema = J::obj(vec![
                ("type", J::s("object")),
                ("required", J::Arr(vec![J::s("error")])),
                ("properties", J::obj(vec![("error", J::obj(vec![("type", J::s("string"))]))])),
            ]);
            schemas.push((ERROR_SCHEMA.to_string(), schema.text()));
        }
        schemas.sort();
        Documented { operations, schemas }
    }

    /// The `operationId` of a route, the rest of its operation object, and whether that refers
    /// to the error schema.
    fn operation(&mut self, route: &Route, owner: Option<Ty>, pending: &mut Vec<Ty>) -> (String, J, bool) {
        let mut op: Vec<(&str, J)> = Vec::new();
        let id = match (&route.handler, owner) {
            (Ok(method), Some(owner)) if self.shared_operation_name(method) => {
                format!("{}_{method}", self.component_name(owner, None))
            }
            _ => route.name.clone(),
        };
        let tags: Vec<String> = if !route.doc.tags.is_empty() {
            route.doc.tags.clone()
        } else if let Some(owner) = owner {
            vec![self.display(owner)]
        } else {
            Vec::new()
        };
        if !tags.is_empty() {
            op.push(("tags", J::Arr(tags.iter().map(|t| J::s(t)).collect())));
        }
        if !route.doc.summary.is_empty() {
            op.push(("summary", J::s(&route.doc.summary)));
        }
        if !route.doc.description.is_empty() {
            op.push(("description", J::s(&route.doc.description)));
        }
        if route.doc.deprecated {
            op.push(("deprecated", J::Bool(true)));
        }

        let mut parameters = Vec::new();
        let mut can_fail = false;
        let mut body = None;
        for binding in &route.params {
            match binding {
                Binding::Path { name, kind } => {
                    can_fail |= *kind != Scalar::Str;
                    parameters.push(J::obj(vec![
                        ("name", J::s(name)),
                        ("in", J::s("path")),
                        ("required", J::Bool(true)),
                        ("schema", scalar_schema(*kind)),
                    ]));
                }
                Binding::Query { name, kind, optional, default } => {
                    let required = !*optional && default.is_none();
                    can_fail |= required || *kind != Scalar::Str;
                    let mut schema = scalar_schema(*kind);
                    if let (J::Obj(pairs), Some(value)) = (&mut schema, default.as_deref().and_then(default_json))
                        && value != J::Null
                    {
                        pairs.push(("default".to_string(), value));
                    }
                    parameters.push(J::obj(vec![
                        ("name", J::s(name)),
                        ("in", J::s("query")),
                        ("required", J::Bool(required)),
                        ("schema", schema),
                    ]));
                }
                Binding::Header { name, optional } | Binding::Cookie { name, optional } => {
                    can_fail |= !*optional;
                    let place = if matches!(binding, Binding::Header { .. }) { "header" } else { "cookie" };
                    parameters.push(J::obj(vec![
                        ("name", J::s(name)),
                        ("in", J::s(place)),
                        ("required", J::Bool(!*optional)),
                        ("schema", J::obj(vec![("type", J::s("string"))])),
                    ]));
                }
                Binding::Body(ty) => {
                    can_fail = true;
                    body = Some(*ty);
                }
                Binding::Request | Binding::Socket => {}
            }
        }
        if !parameters.is_empty() {
            op.push(("parameters", J::Arr(parameters)));
        }
        if let Some(ty) = body {
            let schema = self.schema(ty, pending);
            op.push((
                "requestBody",
                J::obj(vec![
                    ("required", J::Bool(true)),
                    ("content", J::obj(vec![("application/json", J::obj(vec![("schema", schema)]))])),
                ]),
            ));
        }

        let mut responses: Vec<(String, J)> = Vec::new();
        if route.ws {
            responses.push(("101".to_string(), J::obj(vec![("description", J::s(reason(101)))])));
        } else {
            let status = route.status.unwrap_or(if matches!(route.reply, Reply::Empty) { 204 } else { 200 });
            let content = match route.reply {
                Reply::Json => Some(("application/json", self.schema(route.ok, pending))),
                Reply::Text => Some(("text/plain", J::obj(vec![("type", J::s("string"))]))),
                Reply::Empty | Reply::Response => None,
            };
            let mut success = vec![("description", J::s(reason(status)))];
            if let Some((media, schema)) = content {
                success.push(("content", J::obj(vec![(media, J::obj(vec![("schema", schema)]))])));
            }
            responses.push((status.to_string(), J::obj(success)));
        }
        let mut errors: Vec<i64> = route.doc.errors.clone();
        if can_fail && !errors.contains(&400) {
            errors.push(400);
        }
        errors.sort_unstable();
        for status in &errors {
            responses.push((status.to_string(), error_response(*status)));
        }
        op.push(("responses", J::Obj(responses)));
        if !route.doc.security.is_empty() {
            let any_of = route.doc.security.iter().map(|name| J::Obj(vec![(name.clone(), J::Arr(Vec::new()))]));
            op.push(("security", J::Arr(any_of.collect())));
        }
        if route.ws {
            op.push(("x-websocket", J::Bool(true)));
        }
        (id, J::obj(op), !errors.is_empty())
    }

    /// Whether more than one type with routes has a route method of this name, so that the
    /// method's name alone would not identify the operation.
    fn shared_operation_name(&self, method: &str) -> bool {
        self.routes
            .values()
            .filter(|routes| routes.iter().any(|r| r.handler.as_ref().is_ok_and(|m| m == method)))
            .count()
            > 1
    }

    /// The schema of a value of `ty`, referring to components for structs and enums.
    fn schema(&mut self, ty: Ty, pending: &mut Vec<Ty>) -> J {
        let string = || J::obj(vec![("type", J::s("string"))]);
        match self.types.kind(ty).clone() {
            TyKind::Int => J::obj(vec![("type", J::s("integer")), ("format", J::s("int64"))]),
            TyKind::Float => J::obj(vec![("type", J::s("number")), ("format", J::s("double"))]),
            TyKind::Bool => J::obj(vec![("type", J::s("boolean"))]),
            TyKind::Str => string(),
            TyKind::None => J::obj(vec![("type", J::s("null"))]),
            TyKind::Sized(kind) => sized_schema(kind),
            TyKind::Option(inner) => {
                let inner = self.schema(inner, pending);
                match inner {
                    J::Obj(mut pairs) if matches!(pairs.first(), Some((k, J::Str(_))) if k == "type") => {
                        let J::Str(name) = pairs[0].1.clone() else { unreachable!() };
                        pairs[0].1 = J::Arr(vec![J::Str(name), J::s("null")]);
                        J::Obj(pairs)
                    }
                    other => J::obj(vec![("oneOf", J::Arr(vec![other, J::obj(vec![("type", J::s("null"))])]))]),
                }
            }
            TyKind::List(elem) | TyKind::FrozenList(elem) => {
                J::obj(vec![("type", J::s("array")), ("items", self.schema(elem, pending))])
            }
            TyKind::Dict(elem, Ty::NONE) => J::obj(vec![
                ("type", J::s("array")),
                ("items", self.schema(elem, pending)),
                ("uniqueItems", J::Bool(true)),
            ]),
            TyKind::Dict(_, value) | TyKind::FrozenDict(_, value) => {
                J::obj(vec![("type", J::s("object")), ("additionalProperties", self.schema(value, pending))])
            }
            TyKind::Tuple(items) => {
                let schemas: Vec<J> = items.iter().map(|t| self.schema(*t, pending)).collect();
                J::obj(vec![
                    ("type", J::s("array")),
                    ("prefixItems", J::Arr(schemas)),
                    ("items", J::Bool(false)),
                    ("minItems", J::Int(items.len() as i64)),
                    ("maxItems", J::Int(items.len() as i64)),
                ])
            }
            TyKind::Enum(_) if self.json_value_ty() == Some(ty) => J::Obj(Vec::new()),
            TyKind::Struct(_) | TyKind::Enum(_) => {
                pending.push(ty);
                let name = self.component_name(ty, None);
                J::obj(vec![("$ref", J::Str(format!("#/components/schemas/{name}")))])
            }
            // The route checker accepts only JSON-capable types.
            _ => J::Obj(Vec::new()),
        }
    }

    /// The schema of a struct or enum, for `components.schemas`.
    fn component(&mut self, ty: Ty, pending: &mut Vec<Ty>) -> J {
        match self.types.kind(ty).clone() {
            TyKind::Struct(id) => {
                let info = &self.structs[id.0];
                let strict = info.json_strict;
                let fields: Vec<(String, Ty, Option<J>)> = info
                    .fields
                    .iter()
                    .map(|f| {
                        (
                            f.json_name.clone().unwrap_or_else(|| f.name.clone()),
                            f.ty,
                            f.default.as_ref().and_then(literal_json),
                        )
                    })
                    .collect();
                let has_default: Vec<bool> = info.fields.iter().map(|f| f.default.is_some()).collect();
                let mut properties = Vec::new();
                let mut required = Vec::new();
                for (index, (name, field_ty, default)) in fields.into_iter().enumerate() {
                    let mut schema = self.schema(field_ty, pending);
                    if let Some(value) = default {
                        schema = with_default(schema, value);
                    }
                    if !has_default[index] && !matches!(self.types.kind(field_ty), TyKind::Option(_)) {
                        required.push(J::s(&name));
                    }
                    properties.push((name, schema));
                }
                let mut object = vec![("type", J::s("object"))];
                if !required.is_empty() {
                    object.push(("required", J::Arr(required)));
                }
                object.push(("properties", J::Obj(properties)));
                if strict {
                    object.push(("additionalProperties", J::Bool(false)));
                }
                J::obj(object)
            }
            TyKind::Enum(id) => {
                let variants: Vec<(String, Vec<(String, Ty)>)> = self.enums[id.0]
                    .variants
                    .iter()
                    .map(|v| {
                        (
                            v.name.clone(),
                            v.fields
                                .iter()
                                .map(|f| (f.json_name.clone().unwrap_or_else(|| f.name.clone()), f.ty))
                                .collect(),
                        )
                    })
                    .collect();
                if variants.iter().all(|(_, fields)| fields.is_empty()) {
                    let names = variants.iter().map(|(name, _)| J::s(name)).collect();
                    return J::obj(vec![("type", J::s("string")), ("enum", J::Arr(names))]);
                }
                let mut branches = Vec::new();
                for (name, fields) in variants {
                    let mut properties = vec![("type".to_string(), J::obj(vec![("const", J::s(&name))]))];
                    let mut required = vec![J::s("type")];
                    for (field_name, field_ty) in fields {
                        if !matches!(self.types.kind(field_ty), TyKind::Option(_)) {
                            required.push(J::s(&field_name));
                        }
                        properties.push((field_name, self.schema(field_ty, pending)));
                    }
                    branches.push(J::obj(vec![
                        ("type", J::s("object")),
                        ("title", J::s(&name)),
                        ("required", J::Arr(required)),
                        ("properties", J::Obj(properties)),
                    ]));
                }
                J::obj(vec![
                    ("oneOf", J::Arr(branches)),
                    ("discriminator", J::obj(vec![("propertyName", J::s("type"))])),
                ])
            }
            _ => J::Obj(Vec::new()),
        }
    }

    /// The name of a struct or enum under `components.schemas`: its name as written, with the
    /// type arguments of a generic type joined by `_` (`Page_Todo`). With `report`, the name is
    /// claimed for `ty`, and a different type that already has it is an error reported there.
    fn component_name(&mut self, ty: Ty, report: Option<Span>) -> String {
        let shown = self.display(ty);
        let mut name = String::new();
        for c in shown.chars() {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                name.push(c);
            } else if c == '?' {
                name.push_str("_opt");
            } else if !name.ends_with('_') && c != ' ' && c != ']' {
                name.push('_');
            }
        }
        let name = name.trim_end_matches('_').to_string();
        let Some(span) = report else { return name };
        let other = if name == ERROR_SCHEMA {
            Some("the error responses' `{\"error\": ...}` body".to_string())
        } else {
            match self.openapi_names.get(&name) {
                Some(&other) if other != ty => Some(format!("`{}`", self.display(other))),
                Some(_) => None,
                None => {
                    self.openapi_names.insert(name.clone(), ty);
                    None
                }
            }
        };
        if let Some(other) = other
            && self.openapi_clashes.insert(name.clone())
        {
            self.error(
                Diagnostic::error(
                    codes::DOC_NAME_CLASH,
                    format!("two schemas are both called `{name}` in the API documentation"),
                    span,
                    format!("documents `{shown}`"),
                )
                .with_help(format!("{other} is documented under the same name; rename one of them")),
            );
        }
        name
    }
}

fn error_response(status: i64) -> J {
    J::obj(vec![
        ("description", J::s(reason(status))),
        (
            "content",
            J::obj(vec![(
                "application/json",
                J::obj(vec![(
                    "schema",
                    J::obj(vec![("$ref", J::Str(format!("#/components/schemas/{ERROR_SCHEMA}")))]),
                )]),
            )]),
        ),
    ])
}

fn scalar_schema(kind: Scalar) -> J {
    match kind {
        Scalar::Int => J::obj(vec![("type", J::s("integer")), ("format", J::s("int64"))]),
        Scalar::Float => J::obj(vec![("type", J::s("number")), ("format", J::s("double"))]),
        Scalar::Bool => J::obj(vec![("type", J::s("boolean"))]),
        Scalar::Str => J::obj(vec![("type", J::s("string"))]),
    }
}

fn sized_schema(kind: SizedKind) -> J {
    let (format, minimum) = match kind {
        SizedKind::I8 | SizedKind::I16 | SizedKind::I32 => ("int32", None),
        SizedKind::U8 | SizedKind::U16 | SizedKind::U32 => ("int64", Some(0)),
        SizedKind::U64 => ("uint64", Some(0)),
        SizedKind::F32 => return J::obj(vec![("type", J::s("number")), ("format", J::s("float"))]),
    };
    let mut pairs = vec![("type", J::s("integer")), ("format", J::s(format))];
    if let Some(minimum) = minimum {
        pairs.push(("minimum", J::Int(minimum)));
    }
    J::obj(pairs)
}

/// Add `default` to a schema; a `$ref` cannot have siblings in every tool, so it is wrapped.
fn with_default(schema: J, value: J) -> J {
    match schema {
        J::Obj(mut pairs) if schema_is_plain(&pairs) => {
            pairs.push(("default".to_string(), value));
            J::Obj(pairs)
        }
        other => J::obj(vec![("allOf", J::Arr(vec![other])), ("default", value)]),
    }
}

fn schema_is_plain(pairs: &[(String, J)]) -> bool {
    !pairs.iter().any(|(k, _)| k == "$ref")
}

/// A field's default as JSON, when it is a literal: numbers, strings, booleans, `None`, and
/// empty lists and dicts.
fn literal_json(expr: &hir::Expr) -> Option<J> {
    Some(match &expr.kind {
        K::Int(v) => J::Int(*v),
        K::Float(v) if v.is_finite() => J::Num(format!("{v:?}")),
        K::Bool(b) => J::Bool(*b),
        K::Str(text) => J::Str(text.clone()),
        K::None => J::Null,
        K::Unary { op: hir::UnaryOp::IntNeg | hir::UnaryOp::FloatNeg, operand } => match literal_json(operand)? {
            J::Int(v) => J::Int(v.checked_neg()?),
            J::Num(text) => J::Num(format!("-{text}")),
            _ => return None,
        },
        K::List(items) if items.is_empty() => J::Arr(Vec::new()),
        _ => return None,
    })
}

/// A query parameter's default, from the source text the route checker kept (`50`, `True`,
/// `"x"` as Rust's `{:?}` writes it, `-1.5`, `None`).
fn default_json(text: &str) -> Option<J> {
    Some(match text {
        "True" => J::Bool(true),
        "False" => J::Bool(false),
        "None" => J::Null,
        _ if text.starts_with('"') => J::Str(unquote(text)?),
        _ => match text.parse::<i64>() {
            Ok(v) => J::Int(v),
            Err(_) => J::Num(text.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| format!("{v:?}"))?),
        },
    })
}

/// Undo Rust's `{:?}` of a string (the escapes it writes for printable text).
fn unquote(text: &str) -> Option<String> {
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'u' => {
                let rest: String = chars.by_ref().take_while(|c| *c != '}').collect();
                let code = u32::from_str_radix(rest.trim_start_matches('{'), 16).ok()?;
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_text_escapes_what_it_must() {
        let value = J::obj(vec![("a\"b", J::s("line\nbreak\t\u{1}\\")), ("n", J::Arr(vec![J::Null, J::Int(-3)]))]);
        assert_eq!(value.text(), r#"{"a\"b":"line\nbreak\t\u0001\\","n":[null,-3]}"#);
        assert_eq!(source_literal(r#"{"a":"\"x\\"}"#), r#""{\"a\":\"\\\"x\\\\\"}""#);
    }

    #[test]
    fn query_defaults_become_json() {
        assert_eq!(default_json("50"), Some(J::Int(50)));
        assert_eq!(default_json("-1.5"), Some(J::Num("-1.5".into())));
        assert_eq!(default_json("True"), Some(J::Bool(true)));
        assert_eq!(default_json("None"), Some(J::Null));
        assert_eq!(default_json(&format!("{:?}", "a \"b\"\n")), Some(J::s("a \"b\"\n")));
    }

    #[test]
    fn statuses_have_reasons() {
        assert_eq!(reason(404), "Not Found");
        assert_eq!(reason(299), "Success");
        assert_eq!(reason(499), "Client Error");
    }
}
