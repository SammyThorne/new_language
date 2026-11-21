//! HTTP routes (docs/05-standard-library.md, "Route annotations" to "Compile errors specific to
//! routes").
//!
//! `@http.get("/todos/{id}")` and its siblings mark functions and methods as route handlers.
//! Every annotated handler is checked when its signature is known: each parameter must match a
//! binding rule (the request itself, a `{name}` path segment, a JSON body, or a query
//! parameter), and the return type must be something that becomes a response.
//!
//! `server.mount(value)` turns every route of the value's type into a call-style handler. The
//! adapters are generated as source code, which is then checked like code the user wrote: for
//! `def get(self, id: int) -> Result[Todo]` on `"/todos/{id}"` the adapter is
//!
//! ```text
//! def __route_0(__req: Request) -> Result[Response]:
//!     __a0 = __req.param_int("id")?
//!     __result = __value.get(__a0)?
//!     return __http.reply_json(__result, 200)
//! __server.route("GET", __prefix + "/todos/{id}", __route_0)
//! ```
//!
//! The types the generated code names are bound to temporary names (`__g0`, ...) in the scope of
//! the `mount` call, so it works whatever the user imported.

use super::openapi;
use super::{Checker, FnCx, Global, error_expr};
use crate::hir::{self, ExprKind as K, FuncId};
use crate::types::{Ty, TyKind};
use lang_diag::{Diagnostic, Diagnostics, Span, codes};
use lang_syntax::ast;

fn mk(kind: hir::ExprKind, ty: Ty, span: Span) -> hir::Expr {
    hir::Expr { kind, ty, span }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct HttpTypes {
    module: usize,
    server: Ty,
    group: Ty,
    request: Ty,
    response: Ty,
    websocket: Ty,
    header: Ty,
    cookie: Ty,
    upload: Ty,
    /// The generic struct `Form[T]`.
    form: usize,
}

/// The `auth` module's types, when the program imports it (docs/13).
#[derive(Clone, Copy, Debug)]
pub(super) struct AuthTypes {
    module: usize,
    user: Ty,
}

/// The security scheme protected routes name in the API documentation; `auth` registers it.
const SIGN_IN_SCHEME: &str = "googleSession";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scalar {
    Int,
    Float,
    Bool,
    Str,
    /// A type with a string form (`JsonString`), such as `uuid.Uuid` or `datetime.Date`, read
    /// from the text through `from_json_string`.
    Text(Ty),
}

impl Scalar {
    fn suffix(self) -> &'static str {
        match self {
            Scalar::Int => "int",
            Scalar::Float => "float",
            Scalar::Bool => "bool",
            Scalar::Str => "str",
            Scalar::Text(_) => "text",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum UploadKind {
    One,
    Optional,
    List,
}

impl UploadKind {
    pub(super) fn suffix(&self) -> &'static str {
        match self {
            UploadKind::One => "",
            UploadKind::Optional => "_opt",
            UploadKind::List => "_list",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum Binding {
    Request,
    /// The connection, in a WebSocket handler.
    Socket,
    /// `user_agent: http.Header`: the header `User-Agent`.
    Header {
        name: String,
        optional: bool,
    },
    /// `session: http.Cookie`: the cookie `session`.
    Cookie {
        name: String,
        optional: bool,
    },
    Path {
        name: String,
        kind: Scalar,
    },
    Body(Ty),
    /// `signup: http.Form[T]`: the form's text fields, decoded into `T`.
    Form(Ty),
    /// `avatar: http.Upload`, `http.Upload?` or `list[http.Upload]`: the files sent as `avatar`.
    Upload {
        name: String,
        kind: UploadKind,
    },
    /// `user: auth.User`, or `auth.User?`: the signed-in user.
    User {
        optional: bool,
    },
    /// `default` is the literal default value's source text.
    Query {
        name: String,
        kind: Scalar,
        optional: bool,
        default: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub(super) enum Reply {
    Response,
    Json,
    Text,
    Empty,
}

#[derive(Clone, Debug)]
pub(super) struct Route {
    pub method: String,
    pub path: String,
    pub status: Option<i64>,
    /// The method's name, or the function.
    pub handler: Result<String, FuncId>,
    /// The method's or function's name.
    pub name: String,
    pub params: Vec<Binding>,
    pub reply: Reply,
    /// What a successful call returns: the return type, or the `Ok` side of a `Result`.
    pub ok: Ty,
    pub fallible: bool,
    /// An `@http.ws` endpoint.
    pub ws: bool,
    /// Refuses signed-out requests: `@auth.required`, or an `auth.User` parameter.
    pub protected: bool,
    pub span: Span,
    /// What the documentation says about it (docs/12): `tags=`, `deprecated=`, `errors=`, and
    /// the handler's docstring split into a summary line and the rest.
    pub doc: RouteDoc,
    /// `max_body=`: the largest request body the route reads, in bytes.
    pub max_body: Option<i64>,
    /// `timeout=`: the seconds its handler may run.
    pub timeout: Option<f64>,
    /// `rate_limit=`: how often each client may call it, such as `"5/minute"`.
    pub rate_limit: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct RouteDoc {
    pub tags: Vec<String>,
    pub deprecated: bool,
    pub errors: Vec<i64>,
    /// The names of the security schemes, any one of which the operation accepts.
    pub security: Vec<String>,
    pub summary: String,
    pub description: String,
}

const ROUTE_METHODS: &[&str] = &["get", "post", "put", "patch", "delete"];
/// The `Server` and `Group` methods that register handlers, which must be shareable.
const REGISTRATION: &[&str] =
    &["route", "get", "post", "put", "patch", "delete", "head", "options", "use", "on_error", "ws", "health"];

impl<'m> Checker<'_, 'm> {
    /// Find the `http` module's types if the program imports it.
    pub(super) fn find_http_types(&mut self) {
        let Some(module) = self.module_names.iter().position(|n| n == "http") else { return };
        let struct_ty = |this: &Self, name: &str| match this.exports[module].get(name) {
            Some((Global::Struct(id), _, _)) => Some(this.structs[id.0].ty),
            _ => None,
        };
        let (Some(server), Some(group), Some(request), Some(response), Some(websocket), Some(header), Some(cookie)) = (
            struct_ty(self, "Server"),
            struct_ty(self, "Group"),
            struct_ty(self, "Request"),
            struct_ty(self, "Response"),
            struct_ty(self, "WebSocket"),
            struct_ty(self, "Header"),
            struct_ty(self, "Cookie"),
        ) else {
            return;
        };
        let Some(upload) = struct_ty(self, "Upload") else { return };
        let Some((Global::GenericStruct(form), _, _)) = self.exports[module].get("Form") else { return };
        let form = *form;
        self.http =
            Some(HttpTypes { module, server, group, request, response, websocket, header, cookie, upload, form });
    }

    /// Find the `auth` module's types if the program imports it.
    pub(super) fn find_auth_types(&mut self) {
        let Some(module) = self.module_names.iter().position(|n| n == "auth") else { return };
        if let Some((Global::Struct(id), _, _)) = self.exports[module].get("User") {
            self.auth = Some(AuthTypes { module, user: self.structs[id.0].ty });
        }
    }

    /// Whether `annotation` is `@auth.required`, after reporting what is wrong with it.
    pub(super) fn auth_annotation(&mut self, annotation: &ast::Annotation) -> bool {
        if annotation.name.name != "auth.required" {
            return false;
        }
        if self.auth.is_none() {
            self.error(
                Diagnostic::error(
                    codes::BAD_ANNOTATION,
                    "`@auth.required` needs the `auth` module",
                    annotation.name.span,
                    "not imported",
                )
                .with_help("add `import auth`"),
            );
        } else if !annotation.args.is_empty() {
            self.error(
                Diagnostic::error(codes::BAD_SIGN_IN, "`@auth.required` takes no arguments", annotation.span, "arguments given")
                    .with_help("restrict who can sign in with `auth.google(allowed_domains=..., allowed_emails=...)`, or check `user` in the handler"),
            );
        }
        self.record_required(annotation);
        true
    }

    /// For the language server: hovering over `@auth.required` shows `auth.required`.
    fn record_required(&mut self, annotation: &ast::Annotation) {
        let Some(auth) = self.auth else { return };
        if self.index.is_none() {
            return;
        }
        let func = match self.exports[auth.module].get("required") {
            Some((Global::Function(func), _, _)) => *func,
            _ => return,
        };
        let (params, ret, definition) = {
            let signature = &self.signatures[func.0];
            (signature.params.iter().map(|p| p.ty).collect(), signature.ret, signature.name_span)
        };
        let ty = self.types.func(params, ret);
        let doc = self
            .fn_items
            .iter()
            .find(|(_, slot)| matches!(slot, super::FnSlot::Plain(id) if *id == func))
            .and_then(|(f, _)| docstring(f))
            .map(|text| {
                let (summary, description) = split_docstring(text);
                if description.is_empty() { summary } else { format!("{summary}\n\n{description}") }
            });
        self.record_with_doc(annotation.name.span, Some(ty), Some(definition), doc);
    }

    /// The sign-in errors of a function marked `@auth.required` without a route.
    fn required_without_route(&mut self, function: &ast::Function) {
        if function.annotations.iter().any(|a| a.name.name.starts_with("http.")) {
            return;
        }
        if let Some(annotation) = function.annotations.iter().find(|a| a.name.name == "auth.required") {
            self.error(
                Diagnostic::error(
                    codes::BAD_SIGN_IN,
                    format!("`@auth.required` is on `{}`, which is not a route", function.name.name),
                    annotation.span,
                    "no `@http` annotation",
                )
                .with_help("`@auth.required` protects a route; for a call-style handler use `auth.protect(handler)`"),
            );
        }
    }

    /// How a path or query parameter of type `ty` is read from its text, if it can be.
    fn scalar_of(&self, ty: Ty) -> Option<Scalar> {
        match ty {
            Ty::INT => Some(Scalar::Int),
            Ty::FLOAT => Some(Scalar::Float),
            Ty::BOOL => Some(Scalar::Bool),
            Ty::STR => Some(Scalar::Str),
            _ if matches!(self.types.kind(ty), TyKind::Struct(_) | TyKind::Enum(_))
                && self.implements(ty, self.builtin().json_string) =>
            {
                Some(Scalar::Text(ty))
            }
            _ => None,
        }
    }

    pub(super) fn in_http_module(&self) -> bool {
        self.http.is_some_and(|h| h.module == self.current_module)
    }

    /// `__http_serve(listener, handler)`, used by `Server.serve`.
    pub(super) fn http_serve_intrinsic(&mut self, cx: &mut FnCx, args: &[ast::Arg], span: Span) -> hir::Expr {
        let [listener, handler] = args else { return error_expr(span) };
        let listener = self.expect(cx, &listener.value, Ty::INT, None);
        let handler_ty = self.types.func(vec![Ty::INT], Ty::NONE);
        let handler = self.expect(cx, &handler.value, handler_ty, None);
        mk(K::Builtin { op: hir::Builtin::HttpServe, args: vec![listener, handler] }, Ty::NONE, span)
    }

    fn route_error(&mut self, message: impl Into<String>, span: Span, label: impl Into<String>, help: Option<&str>) {
        let mut diagnostic = Diagnostic::error(codes::BAD_ROUTE, message, span, label);
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(help);
        }
        self.error(diagnostic);
    }

    fn form_conflict(&mut self, message: String, span: Span, label: &str) {
        self.error(
            Diagnostic::error(codes::FORM_CONFLICT, message, span, label).with_help(
                "a request has one body: take a JSON body, or the form's fields (`http.Form[T]`) and files (`http.Upload`), or `http.Request` and read it yourself",
            ),
        );
    }

    /// What an `http.Form[T]`, `http.Upload`, `http.Upload?` or `list[http.Upload]` parameter
    /// binds (with the upload's name still to fill in); `Err(T)` for a form whose `T` is not
    /// decodable; `None` for any other type.
    fn form_binding(&mut self, ty: Ty) -> Option<Result<Binding, Ty>> {
        let http = self.http?;
        if let Some((template, args)) = self.instance_of.get(&ty)
            && *template == http.form
        {
            let inner = args[0];
            return Some(if self.json_capable_for(inner, self.builtin().from_json) {
                Ok(Binding::Form(inner))
            } else {
                Err(inner)
            });
        }
        let kind = if ty == http.upload {
            UploadKind::One
        } else if self.types.option_inner(ty) == Some(http.upload) {
            UploadKind::Optional
        } else if self.types.list_elem(ty) == Some(http.upload) {
            UploadKind::List
        } else {
            return None;
        };
        Some(Ok(Binding::Upload { name: String::new(), kind }))
    }

    // ---- checking annotated handlers ----------------------------------------------------

    /// Check every `@http.*` annotation and record the routes.
    pub(super) fn check_routes(&mut self) {
        for (function, slot) in self.fn_items.clone() {
            if self.auth.is_some() {
                self.required_without_route(function);
            }
            if !function.annotations.iter().any(|a| a.name.name.starts_with("http.")) {
                continue;
            }
            self.enter_module(self.module_of(function.name.span));
            let super::FnSlot::Plain(func) = slot else { continue };
            if let Some(route) = self.route_of(function, func, false) {
                self.function_routes.insert(func, route);
            }
        }
        for imp in self.impl_items.clone() {
            if imp.trait_name.is_some() || !imp.generics.is_empty() {
                continue;
            }
            self.enter_module(self.module_of(imp.target.span));
            let target = match self.globals.get(&imp.target.name) {
                Some(&(Global::Struct(id), _)) => self.structs[id.0].ty,
                Some(&(Global::Enum(id), _)) => self.enums[id.0].ty,
                _ => continue,
            };
            let mut seen: Vec<(String, String, Span)> = Vec::new();
            for method in &imp.methods {
                if self.auth.is_some() {
                    self.required_without_route(method);
                }
                if !method.annotations.iter().any(|a| a.name.name.starts_with("http.")) {
                    continue;
                }
                let Some(&func) = self.inherent.get(&(target, method.name.name.clone())) else { continue };
                let Some(route) = self.route_of(method, func, true) else { continue };
                if let Some((_, _, first)) = seen.iter().find(|(m, p, _)| *m == route.method && *p == route.path) {
                    let first = *first;
                    self.error(
                        Diagnostic::error(
                            codes::BAD_ROUTE,
                            format!("two handlers for {} {}", route.method, route.path),
                            route.span,
                            "declared again here",
                        )
                        .with_label(first, "first declared here"),
                    );
                    continue;
                }
                seen.push((route.method.clone(), route.path.clone(), route.span));
                self.routes.entry(target).or_default().push(route);
            }
        }
    }

    /// The route an annotated function or method declares, or `None` after reporting why not.
    fn route_of(&mut self, function: &ast::Function, func: FuncId, is_method: bool) -> Option<Route> {
        let annotation = function.annotations.iter().find(|a| a.name.name.starts_with("http."))?;
        let span = annotation.span;
        if self.http.is_none() {
            self.route_error("`@http` routes need the `http` module", span, "not imported", Some("add `import http`"));
            return None;
        }
        let verb = annotation.name.name.trim_start_matches("http.");
        let ws = verb == "ws";
        if !ROUTE_METHODS.contains(&verb) && !ws {
            self.route_error(
                format!("unknown annotation `@{}`", annotation.name.name),
                annotation.name.span,
                "not a route annotation",
                Some("the route annotations are `@http.get`, `@http.post`, `@http.put`, `@http.patch`, `@http.delete` and `@http.ws`"),
            );
            return None;
        }
        let method = if ws { "GET".to_string() } else { verb.to_ascii_uppercase() };
        let mut path = None;
        let mut status = None;
        let mut doc = RouteDoc::default();
        let mut max_body = None;
        let mut timeout = None;
        let mut rate_limit = None;
        for arg in &annotation.args {
            match (&arg.keyword, &arg.value) {
                (None, ast::AnnotationValue::Str(text)) if path.is_none() => path = Some(text.clone()),
                (Some(k), ast::AnnotationValue::Int(code))
                    if k.name == "status" && (100..600).contains(code) && !ws =>
                {
                    status = Some(*code)
                }
                (Some(k), value) if k.name == "tags" => {
                    let tags = match value {
                        ast::AnnotationValue::List(items) => items
                            .iter()
                            .map(|item| match item {
                                ast::AnnotationValue::Str(text) if !text.is_empty() => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>(),
                        _ => None,
                    };
                    let Some(tags) = tags else {
                        self.route_error(
                            "`tags=` takes a list of strings",
                            arg.span,
                            "not a list of strings",
                            Some("for example `tags=[\"todos\"]`"),
                        );
                        return None;
                    };
                    doc.tags = tags;
                }
                (Some(k), value) if k.name == "security" && !ws => {
                    let names = match value {
                        ast::AnnotationValue::List(items) => items
                            .iter()
                            .map(|item| match item {
                                ast::AnnotationValue::Str(text)
                                    if !text.is_empty() && !text.contains(char::is_whitespace) =>
                                {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>(),
                        _ => None,
                    };
                    let Some(mut names) = names else {
                        self.route_error(
                            "`security=` takes a list of security scheme names",
                            arg.span,
                            "not a list of names",
                            Some(
                                "for example `security=[\"token\"]`, with `token` given to `server.docs(security=...)`",
                            ),
                        );
                        return None;
                    };
                    let mut unique: Vec<String> = Vec::new();
                    names.retain(|name| {
                        let new = !unique.contains(name);
                        unique.push(name.clone());
                        new
                    });
                    doc.security = names;
                }
                (Some(k), value) if k.name == "max_body" && !ws => match value {
                    ast::AnnotationValue::Int(bytes) if (1..1 << 38).contains(bytes) => max_body = Some(*bytes),
                    _ => {
                        self.route_error(
                            "`max_body=` takes the largest body to accept, in bytes",
                            arg.span,
                            "not a positive number of bytes",
                            Some("for example `max_body=10485760` for 10 MiB"),
                        );
                        return None;
                    }
                },
                (Some(k), value) if k.name == "timeout" && !ws => {
                    let seconds = match value {
                        ast::AnnotationValue::Int(n) => Some(*n as f64),
                        ast::AnnotationValue::Float(x) => Some(*x),
                        _ => None,
                    };
                    match seconds {
                        Some(seconds) if seconds > 0.0 && seconds <= 1e9 => timeout = Some(seconds),
                        _ => {
                            self.route_error(
                                "`timeout=` takes the seconds the handler may run",
                                arg.span,
                                "not a positive number of seconds",
                                Some("for example `timeout=10.0`"),
                            );
                            return None;
                        }
                    }
                }
                (Some(k), value) if k.name == "rate_limit" && !ws => match value {
                    ast::AnnotationValue::Str(text) if valid_rate(text) => rate_limit = Some(text.clone()),
                    _ => {
                        self.route_error(
                            "`rate_limit=` takes how many requests a client may make in a period",
                            arg.span,
                            "not a rate such as \"5/minute\"",
                            Some("for example `rate_limit=\"5/minute\"`; the period is `second`, `minute`, `hour` or `day`"),
                        );
                        return None;
                    }
                },
                (Some(k), value) if k.name == "deprecated" => {
                    let ast::AnnotationValue::Bool(flag) = value else {
                        self.route_error(
                            "`deprecated=` takes `True` or `False`",
                            arg.span,
                            "not a boolean",
                            Some("for example `deprecated=True`"),
                        );
                        return None;
                    };
                    doc.deprecated = *flag;
                }
                (Some(k), value) if k.name == "errors" && !ws => {
                    let errors = match value {
                        ast::AnnotationValue::List(items) => items
                            .iter()
                            .map(|item| match item {
                                ast::AnnotationValue::Int(code) if (400..600).contains(code) => Some(*code),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>(),
                        _ => None,
                    };
                    let Some(mut errors) = errors else {
                        self.route_error(
                            "`errors=` takes a list of error statuses, from 400 to 599",
                            arg.span,
                            "not a list of error statuses",
                            Some("for example `errors=[404, 409]`"),
                        );
                        return None;
                    };
                    errors.sort_unstable();
                    errors.dedup();
                    doc.errors = errors;
                }
                _ => {
                    self.route_error(
                        if ws {
                            "`@http.ws` takes a path and, optionally, `tags=` and `deprecated=`"
                        } else {
                            "a route annotation takes a path and, optionally, `status=`, `tags=`, `deprecated=`, `errors=`, `security=`, `max_body=`, `timeout=` and `rate_limit=`"
                        },
                        arg.span,
                        "not expected here",
                        Some(if arg.keyword.as_ref().is_some_and(|k| k.name == "summary" || k.name == "description") {
                            "the summary and description come from the handler's docstring: its first line, and the rest"
                        } else if ws {
                            "for example `@http.ws(\"/chat\")`"
                        } else {
                            "for example `@http.post(\"/todos\", status=201)`"
                        }),
                    );
                    return None;
                }
            }
        }
        let Some(path) = path else {
            self.route_error(
                "a route annotation needs a path",
                span,
                "path missing",
                Some("for example `@http.get(\"/todos\")`"),
            );
            return None;
        };
        let names = self.path_names(&path, span)?;
        if is_method && function.self_param.is_none() {
            self.route_error("a route method must take `self`", function.name.span, "no `self`", Some("routes of a value are mounted with `server.mount(value)`; use a top-level function for one without state"));
            return None;
        }
        let signature = self.signatures[func.0].clone();
        let mut params = Vec::new();
        let mut body: Option<String> = None;
        // The first parameter read from a form, and the `http.Form[T]` one.
        let mut form: Option<String> = None;
        let mut form_fields: Option<String> = None;
        let mut used_names = Vec::new();
        for (index, param) in signature.params.iter().enumerate() {
            let ast_param = &function.params[index];
            let user = self.auth.and_then(|a| {
                if param.ty == a.user {
                    Some(false)
                } else if self.types.option_inner(param.ty) == Some(a.user) {
                    Some(true)
                } else {
                    None
                }
            });
            let binding = if Some(param.ty) == self.http.map(|h| h.request) {
                Binding::Request
            } else if let Some(optional) = user {
                if ws {
                    let mut diagnostic = Diagnostic::error(
                        codes::BAD_SIGN_IN,
                        format!(
                            "parameter `{}` is the signed-in user, which a WebSocket handler cannot have yet",
                            param.name
                        ),
                        param.hint_span,
                        "in a WebSocket handler",
                    );
                    diagnostic = diagnostic.with_help("take an `http.Request` parameter as well, and call `auth.user(req)` with it before the handler talks on the socket");
                    self.error(diagnostic);
                    return None;
                }
                if params.iter().any(|b| matches!(b, Binding::User { .. })) {
                    self.error(
                        Diagnostic::error(
                            codes::BAD_SIGN_IN,
                            format!("`{}` has two parameters for the signed-in user", function.name.name),
                            param.hint_span,
                            "second one",
                        )
                        .with_help("one `auth.User` parameter (or `auth.User?`) gets the user"),
                    );
                    return None;
                }
                Binding::User { optional }
            } else if Some(param.ty) == self.http.map(|h| h.websocket) {
                if !ws {
                    self.route_error(
                        format!("parameter `{}` is a WebSocket, but this is not a WebSocket endpoint", param.name),
                        param.hint_span,
                        "only under `@http.ws`",
                        Some("declare WebSocket endpoints with `@http.ws(\"/path\")`"),
                    );
                    return None;
                }
                if params.iter().any(|b| matches!(b, Binding::Socket)) {
                    self.route_error(
                        "a WebSocket handler takes one `http.WebSocket`",
                        param.hint_span,
                        "second one",
                        None,
                    );
                    return None;
                }
                Binding::Socket
            } else if let Some((cookie, optional)) = self.http.and_then(|h| {
                let inner = self.types.option_inner(param.ty);
                [(h.header, false), (h.cookie, true)].into_iter().find_map(|(ty, cookie)| {
                    if param.ty == ty {
                        Some((cookie, false))
                    } else if inner == Some(ty) {
                        Some((cookie, true))
                    } else {
                        None
                    }
                })
            }) {
                if cookie {
                    Binding::Cookie { name: param.name.clone(), optional }
                } else {
                    // `user_agent` is the header `User-Agent`.
                    let name = param
                        .name
                        .split('_')
                        .filter(|w| !w.is_empty())
                        .map(|w| {
                            let mut chars = w.chars();
                            chars
                                .next()
                                .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join("-");
                    Binding::Header { name, optional }
                }
            } else if let Some(binding) = self.form_binding(param.ty) {
                let binding = match binding {
                    Ok(binding) => binding,
                    Err(inner) => {
                        let shown = self.display(inner);
                        self.route_error(
                            format!("the fields of form `{}` cannot be decoded into a `{shown}`", param.name),
                            param.hint_span,
                            "not decodable",
                            Some(&format!("add `@derive(Json)` to `{shown}`")),
                        );
                        return None;
                    }
                };
                let binding = match binding {
                    Binding::Upload { kind, .. } => Binding::Upload { name: param.name.clone(), kind },
                    other => other,
                };
                if let Some(first) = &body {
                    self.form_conflict(
                        format!("`{}` reads the request body as a form, but `{first}` reads it as JSON", param.name),
                        param.hint_span,
                        "a form parameter",
                    );
                    return None;
                }
                if method == "GET" || method == "DELETE" || ws {
                    let what = if ws { "a WebSocket endpoint".to_string() } else { format!("a {method} route") };
                    let mut diagnostic = Diagnostic::error(
                        codes::FORM_CONFLICT,
                        format!("{what} has no request body to read form `{}` from", param.name),
                        param.hint_span,
                        "a form parameter",
                    );
                    diagnostic = diagnostic.with_help("use `@http.post` (or `put` or `patch`); a form sent with GET puts its fields in the query, so take them as query parameters");
                    self.error(diagnostic);
                    return None;
                }
                if matches!(binding, Binding::Form(_)) {
                    if let Some(first) = &form_fields {
                        self.form_conflict(
                            format!("a handler has one form, but `{first}` and `{}` both want its fields", param.name),
                            param.hint_span,
                            "second `http.Form` parameter",
                        );
                        return None;
                    }
                    form_fields = Some(param.name.clone());
                }
                form.get_or_insert_with(|| param.name.clone());
                binding
            } else if names.contains(&param.name) {
                used_names.push(param.name.clone());
                match self.scalar_of(param.ty) {
                    Some(kind) => Binding::Path { name: param.name.clone(), kind },
                    None => {
                        let shown = self.display(param.ty);
                        self.route_error(
                            format!("path parameter `{}` cannot be a `{shown}`", param.name),
                            param.hint_span,
                            "not parsed from a path",
                            Some(
                                "path parameters are `int`, `float`, `bool`, `str`, or a type with a string form (`JsonString`) such as `uuid.Uuid` or `datetime.Date`",
                            ),
                        );
                        return None;
                    }
                }
            } else if let Some((kind, optional)) = self
                .scalar_of(param.ty)
                .map(|k| (k, false))
                .or_else(|| self.types.option_inner(param.ty).and_then(|t| self.scalar_of(t)).map(|k| (k, true)))
            {
                let default = match &ast_param.default {
                    None => None,
                    Some(expr) => match self.literal_text(expr) {
                        Some(text) if matches!(kind, Scalar::Text(_)) && text != "None" => {
                            let shown = self.display(param.ty);
                            self.route_error(
                                format!(
                                    "query parameter `{}` is a `{shown}`, so its default can only be `None`",
                                    param.name
                                ),
                                expr.span,
                                "not a default for this type",
                                Some("make it optional with `= None`, and supply the default in the handler"),
                            );
                            return None;
                        }
                        Some(text) => Some(text),
                        None => {
                            self.route_error(
                                format!("the default of query parameter `{}` must be a literal", param.name),
                                expr.span,
                                "not a literal",
                                Some("use a number, string, `True`, `False` or `None`"),
                            );
                            return None;
                        }
                    },
                };
                Binding::Query { name: param.name.clone(), kind, optional, default }
            } else if self.json_capable_for(param.ty, self.builtin().from_json) {
                if let Some(first) = &form {
                    self.form_conflict(
                        format!("`{}` reads the request body as JSON, but `{first}` reads it as a form", param.name),
                        param.hint_span,
                        "a JSON body parameter",
                    );
                    return None;
                }
                if let Some(first) = &body {
                    self.route_error(
                        format!("a handler has one JSON body, but `{first}` and `{}` both want it", param.name),
                        param.hint_span,
                        "second body parameter",
                        None,
                    );
                    return None;
                }
                if method == "GET" || method == "DELETE" {
                    self.route_error(
                        format!("a {method} route has no request body"),
                        param.hint_span,
                        "a body parameter",
                        Some("read data from the path or the query, or use `@http.post` or `@http.put`"),
                    );
                    return None;
                }
                body = Some(param.name.clone());
                Binding::Body(param.ty)
            } else {
                let shown = self.display(param.ty);
                let help = if matches!(self.types.kind(param.ty), TyKind::Struct(_) | TyKind::Enum(_)) {
                    format!("to read it from the JSON body, add `@derive(Json)` to `{shown}`")
                } else {
                    "a parameter is the request (`http.Request`), a `{name}` in the path, a JSON body, a header (`http.Header`), a cookie (`http.Cookie`), a form (`http.Form[T]`) or a file in it (`http.Upload`), or a query parameter of type `int`, `float`, `bool`, `str` or a type with a string form such as `uuid.Uuid`".to_string()
                };
                self.route_error(
                    format!("cannot tell where parameter `{}` comes from", param.name),
                    param.hint_span,
                    format!("a `{shown}`"),
                    Some(&help),
                );
                return None;
            };
            params.push(binding);
        }
        for name in &names {
            if !used_names.contains(name) {
                self.route_error(
                    format!("the path has `{{{name}}}`, but `{}` has no parameter `{name}`", function.name.name),
                    span,
                    format!("`{{{name}}}` is not used"),
                    None,
                );
                return None;
            }
        }
        if ws {
            if !params.iter().any(|b| matches!(b, Binding::Socket)) {
                self.route_error(
                    format!("WebSocket handler `{}` has no `http.WebSocket` parameter", function.name.name),
                    function.name.span,
                    "no connection to talk on",
                    Some("add a parameter such as `ws: http.WebSocket`"),
                );
                return None;
            }
            if signature.ret != Ty::NONE {
                let shown = self.display(signature.ret);
                self.route_error(
                    format!("a WebSocket handler returns nothing, but `{}` returns `{shown}`", function.name.name),
                    signature.ret_span.unwrap_or(function.name.span),
                    "remove the return type",
                    Some("the handler talks to the peer through its `http.WebSocket`; the connection closes when it returns"),
                );
                return None;
            }
        }
        let (fallible, ok) = match self.types.result_parts(signature.ret) {
            Some((ok, _)) => (true, ok),
            None => (false, signature.ret),
        };
        let reply = if ok == Ty::NONE {
            Reply::Empty
        } else if Some(ok) == self.http.map(|h| h.response) {
            Reply::Response
        } else if ok == Ty::STR {
            Reply::Text
        } else if self.json_capable(ok) {
            Reply::Json
        } else {
            let shown = self.display(ok);
            self.route_error(
                format!("a `{shown}` cannot be turned into a response"),
                signature.ret_span.unwrap_or(function.name.span),
                "not a response",
                Some("return `http.Response`, `str`, a type with `@derive(Json)`, nothing, or a `Result` of those"),
            );
            return None;
        };
        let handler = if is_method { Ok(function.name.name.clone()) } else { Err(func) };
        if let Some(text) = docstring(function) {
            (doc.summary, doc.description) = split_docstring(text);
        }
        let protected = (self.auth.is_some() && function.annotations.iter().any(|a| a.name.name == "auth.required"))
            || params.iter().any(|b| matches!(b, Binding::User { optional: false }));
        if protected && !ws {
            if !doc.security.iter().any(|name| name == SIGN_IN_SCHEME) {
                doc.security.push(SIGN_IN_SCHEME.to_string());
            }
            if !doc.errors.contains(&401) {
                doc.errors.push(401);
                doc.errors.sort_unstable();
            }
        }
        let name = function.name.name.clone();
        // A rate-limited route can answer 429; say so in its documentation.
        if rate_limit.is_some() && !doc.errors.contains(&429) {
            doc.errors.push(429);
            doc.errors.sort_unstable();
        }
        Some(Route {
            method,
            path,
            status,
            handler,
            name,
            params,
            reply,
            ok,
            fallible,
            ws,
            protected,
            span,
            doc,
            max_body,
            timeout,
            rate_limit,
        })
    }

    /// The `{name}` segments of a route path, after checking its syntax.
    fn path_names(&mut self, path: &str, span: Span) -> Option<Vec<String>> {
        if !path.starts_with('/') {
            self.route_error(format!("route path `{path}` must start with `/`"), span, "not a path", None);
            return None;
        }
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut names = Vec::new();
        for (index, segment) in segments.iter().enumerate() {
            let Some(inner) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
                if segment.contains('{') || segment.contains('}') {
                    self.route_error(
                        format!("`{segment}` is not a valid path segment"),
                        span,
                        "bad segment",
                        Some("a parameter is a whole segment: `/todos/{id}`"),
                    );
                    return None;
                }
                continue;
            };
            let (name, rest) = match inner.strip_suffix("...") {
                Some(name) => (name, true),
                None => (inner, false),
            };
            if rest && index + 1 != segments.len() {
                self.route_error(format!("`{{{name}...}}` must be the last segment"), span, "not last", None);
                return None;
            }
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                self.route_error(format!("`{{{inner}}}` is not a valid parameter name"), span, "bad name", None);
                return None;
            }
            if names.iter().any(|n| n == name) {
                self.route_error(format!("the path has `{{{name}}}` twice"), span, "repeated", None);
                return None;
            }
            names.push(name.to_string());
        }
        Some(names)
    }

    /// The source text of a literal default value.
    fn literal_text(&self, expr: &ast::Expr) -> Option<String> {
        Some(match &expr.kind {
            ast::ExprKind::Int(v) => v.to_string(),
            ast::ExprKind::Float(v) => format!("{v:?}"),
            ast::ExprKind::Bool(true) => "True".to_string(),
            ast::ExprKind::Bool(false) => "False".to_string(),
            ast::ExprKind::None => "None".to_string(),
            ast::ExprKind::Str(text) => format!("{text:?}"),
            ast::ExprKind::Unary { op: ast::UnaryOp::Neg, operand } => format!("-{}", self.literal_text(operand)?),
            _ => return None,
        })
    }

    // ---- registration -------------------------------------------------------------------

    pub(super) fn is_http_server(&self, ty: Ty) -> bool {
        self.http.is_some_and(|h| ty == h.server || ty == h.group)
    }

    /// After a call to a `Server` or `Group` method that registers handlers: the handlers serve
    /// requests from several tasks at once, so they must be shareable.
    pub(super) fn check_registration(&mut self, cx: &FnCx, method: &str, call: &hir::Expr) {
        // Generated adapters capture only the mounted value, which `mount` checks. The standard
        // library's own handlers (`auth`'s routes) capture only values it checks where they are
        // given to it, such as `auth.google(on_login=...)`.
        if self.is_stdlib_module(self.current_module)
            || !REGISTRATION.contains(&method)
            || self.generated_files.contains(&call.span.file)
        {
            return;
        }
        let K::Call { args, .. } = &call.kind else { return };
        for arg in args.iter().skip(1) {
            match (&arg.kind, self.types.kind(arg.ty)) {
                (_, TyKind::Func(..)) => {
                    self.require_shareable(cx, arg, "used to serve requests from several tasks");
                }
                // `server.health(checks={...})`: each check runs in a task of its own.
                (K::Dict(entries), TyKind::Dict(_, value)) if matches!(self.types.kind(*value), TyKind::Func(..)) => {
                    for (_, check) in entries {
                        self.require_shareable(cx, check, "run in another task");
                    }
                }
                (_, TyKind::Dict(_, value)) if matches!(self.types.kind(*value), TyKind::Func(..)) => {
                    let shown = self.display(arg.ty);
                    self.error(
                        Diagnostic::error(
                            codes::NOT_SHAREABLE,
                            format!("a value of type `{shown}` cannot be run in other tasks"),
                            arg.span,
                            "the functions in it might capture variables that can be changed".to_string(),
                        )
                        .with_help("write the dict out here, `checks={\"database\": ping}`, so that each function can be checked"),
                    );
                }
                _ => {}
            }
        }
    }

    /// `server.mount(value, prefix="")`: register every route of the value's type (or one
    /// annotated function).
    pub(super) fn mount(&mut self, cx: &mut FnCx, server: hir::Expr, args: &[ast::Arg], span: Span) -> hir::Expr {
        let http = self.http.expect("checked by the caller");
        let (value_arg, prefix_arg) = match args {
            [value] if value.name.is_none() => (value, None),
            [value, prefix] if value.name.is_none() && prefix.name.as_ref().is_none_or(|n| n.name == "prefix") => {
                (value, Some(prefix))
            }
            _ => {
                self.error(Diagnostic::error(
                    codes::WRONG_ARG_COUNT,
                    "`mount` takes a value or function, and optionally `prefix=`",
                    span,
                    "for example `server.mount(app, prefix=\"/api\")`",
                ));
                self.check_all_quietly(cx, args);
                return error_expr(span);
            }
        };
        let value = self.expr(cx, &value_arg.value, None);
        let prefix = match prefix_arg {
            Some(arg) => self.expect(cx, &arg.value, Ty::STR, None),
            None => mk(K::Str(String::new()), Ty::STR, span),
        };
        if self.types.contains_error(value.ty) {
            return error_expr(span);
        }
        let (routes, function) = match &value.kind {
            K::FuncRef(func) if self.function_routes.contains_key(func) => {
                (vec![self.function_routes[func].clone()], Some(*func))
            }
            _ => match self.routes.get(&value.ty) {
                Some(routes) => (routes.clone(), None),
                None => {
                    let shown = self.display(value.ty);
                    self.route_error(
                        format!("a `{shown}` has no `@http` routes to mount"),
                        value.span,
                        "nothing to mount",
                        Some("mount a value whose type has methods marked `@http.get(...)` and so on, or a function marked that way"),
                    );
                    return error_expr(span);
                }
            },
        };
        if function.is_none() && !self.require_shareable(cx, &value, "served by several tasks at once") {
            return error_expr(span);
        }

        // Temporary names for what the generated code refers to.
        let mut aliases: Vec<(String, Global)> = vec![("__http".to_string(), Global::Module(http.module))];
        if let Some(auth) = self.auth
            && routes.iter().any(|r| r.protected || r.params.iter().any(|b| matches!(b, Binding::User { .. })))
        {
            aliases.push(("__auth".to_string(), Global::Module(auth.module)));
        }
        let server_name = self.alias_type(server.ty, &mut aliases);
        let request_name = self.alias_type(http.request, &mut aliases);
        let response_name = self.alias_type(http.response, &mut aliases);
        let socket_name =
            if routes.iter().any(|r| r.ws) { self.alias_type(http.websocket, &mut aliases) } else { String::new() };
        self.generated += 1;
        let mount_name = format!("__mount_{}", self.generated);
        let mut text = String::new();
        match function {
            Some(func) => {
                aliases.push(("__f0".to_string(), Global::Function(func)));
                text.push_str(&format!("def {mount_name}(__server: {server_name}, __prefix: str):\n"));
            }
            None => {
                let value_name = self.alias_type(value.ty, &mut aliases);
                text.push_str(&format!(
                    "def {mount_name}(__server: {server_name}, __value: {value_name}, __prefix: str):\n"
                ));
            }
        }
        let documented =
            self.uses_docs.then(|| self.document_routes(&routes, function.is_none().then_some(value.ty), span));
        for (index, route) in routes.iter().enumerate() {
            if let Some(documented) = &documented {
                let (id, operation, ws) = &documented.operations[index];
                text.push_str(&format!(
                    "    __server.__document({:?}, __prefix + {:?}, {}, {}, {}, {})\n",
                    route.method,
                    route.path.replace("...}", "}"),
                    openapi::source_literal(id),
                    openapi::source_literal(operation),
                    if *ws { "True" } else { "False" },
                    openapi::source_literal(&route.doc.security.join(" "))
                ));
            }
            text.push_str(&format!("    def __route_{index}(__req: {request_name}) -> Result[{response_name}]:\n"));
            if route.protected {
                text.push_str("        match __auth.guard(__req):\n");
                text.push_str("            case Some(__refusal):\n");
                text.push_str("                return Ok(__refusal)\n");
                text.push_str("            case None:\n");
                text.push_str("                pass\n");
            }
            let mut args = Vec::new();
            if route.params.iter().any(|b| matches!(b, Binding::Form(_) | Binding::Upload { .. })) {
                text.push_str("        __form = __http.form_data(__req)?\n");
            }
            for (n, binding) in route.params.iter().enumerate() {
                let line = match binding {
                    Binding::Request => {
                        args.push("__req".to_string());
                        continue;
                    }
                    Binding::Socket => {
                        args.push("__ws".to_string());
                        continue;
                    }
                    Binding::Header { name, optional } => {
                        format!("__http.header_param{}(__req, {name:?})?", if *optional { "_opt" } else { "" })
                    }
                    Binding::Cookie { name, optional } => {
                        format!("__http.cookie_param{}(__req, {name:?})?", if *optional { "_opt" } else { "" })
                    }
                    Binding::Path { name, kind: Scalar::Str } => format!("__req.param({name:?})"),
                    Binding::Path { name, kind: Scalar::Text(ty) } => {
                        let ty_name = self.alias_type(*ty, &mut aliases);
                        format!("__http.path_text[{ty_name}](__req, {name:?})?")
                    }
                    Binding::Path { name, kind } => format!("__req.param_{}({name:?})?", kind.suffix()),
                    Binding::Body(ty) => {
                        let body_name = self.alias_type(*ty, &mut aliases);
                        format!("__http.body[{body_name}](__req)?")
                    }
                    Binding::Form(ty) => {
                        let form_name = self.alias_type(*ty, &mut aliases);
                        format!("__http.form_value[{form_name}](__form)?")
                    }
                    Binding::Upload { name, kind } => {
                        format!("__http.upload_param{}(__form, {name:?})?", kind.suffix())
                    }
                    Binding::User { optional: false } => "__auth.user_param(__req)?".to_string(),
                    Binding::User { optional: true } => "__auth.user(__req)".to_string(),
                    Binding::Query { name, kind: Scalar::Text(ty), optional, .. } => {
                        let ty_name = self.alias_type(*ty, &mut aliases);
                        let suffix = if *optional { "_opt" } else { "" };
                        format!("__http.query_text{suffix}[{ty_name}](__req, {name:?})?")
                    }
                    Binding::Query { name, kind, optional: false, default: None } => {
                        format!("__http.query_{}(__req, {name:?})?", kind.suffix())
                    }
                    Binding::Query { name, kind, default: Some(default), .. } if default != "None" => {
                        format!("__http.query_{}_opt(__req, {name:?})?.unwrap_or({default})", kind.suffix())
                    }
                    Binding::Query { name, kind, .. } => {
                        format!("__http.query_{}_opt(__req, {name:?})?", kind.suffix())
                    }
                };
                text.push_str(&format!("        __a{n} = {line}\n"));
                args.push(format!("__a{n}"));
            }
            let call = match &route.handler {
                Ok(method) => format!("__value.{method}({})", args.join(", ")),
                Err(_) => format!("__f0({})", args.join(", ")),
            };
            if route.ws {
                text.push_str(&format!("        def __ws_{index}(__ws: {socket_name}):\n"));
                text.push_str(&format!("            {call}\n"));
                text.push_str(&format!("        return __http.upgrade(__req, __ws_{index})\n"));
                text.push_str(&format!("    __server.route(\"GET\", __prefix + {:?}, __route_{index})\n", route.path));
                continue;
            }
            let try_mark = if route.fallible { "?" } else { "" };
            let status = route.status.unwrap_or(if matches!(route.reply, Reply::Empty) { 204 } else { 200 });
            match route.reply {
                Reply::Empty => {
                    text.push_str(&format!("        {call}{try_mark}\n"));
                    text.push_str(&format!("        return __http.reply_empty({status})\n"));
                }
                Reply::Response => text.push_str(&format!("        return Ok({call}{try_mark})\n")),
                Reply::Text => {
                    text.push_str(&format!("        return __http.reply_text({call}{try_mark}, {status})\n"))
                }
                Reply::Json => {
                    text.push_str(&format!("        return __http.reply_json({call}{try_mark}, {status})\n"))
                }
            }
            let mut limit = route.max_body.map(|bytes| format!(", max_body={bytes}")).unwrap_or_default();
            if let Some(seconds) = route.timeout {
                limit.push_str(&format!(", timeout={seconds:?}"));
            }
            if let Some(rate) = &route.rate_limit {
                limit.push_str(&format!(", rate_limit={rate:?}"));
            }
            text.push_str(&format!(
                "    __server.route({:?}, __prefix + {:?}, __route_{index}{limit})\n",
                route.method, route.path
            ));
        }

        for (name, schema) in documented.iter().flat_map(|d| &d.schemas) {
            text.push_str(&format!(
                "    __server.__schema({}, {})\n",
                openapi::source_literal(name),
                openapi::source_literal(schema)
            ));
        }

        let location = {
            let file = self.sources.get(span.file);
            let (line, col) = file.line_col(span.lo as usize);
            format!("{}:{line}:{col}", file.name)
        };
        let file = self.sources.add(format!("<routes mounted at {location}>"), text);
        self.generated_files.push(file);
        let mut parse_diags = Diagnostics::default();
        let tokens = lang_syntax::lex(file, &self.sources.get(file).text.clone(), &mut parse_diags);
        let module = lang_syntax::parse(&tokens, &mut parse_diags);
        let Some(ast::Item::Function(generated)) = module.items.first() else {
            panic!("the generated route adapters do not parse");
        };
        assert!(!parse_diags.has_errors(), "the generated route adapters do not parse");

        let saved: Vec<(String, Option<(Global, Span)>)> = aliases
            .iter()
            .map(|(name, global)| (name.clone(), self.globals.insert(name.clone(), (*global, span))))
            .collect();
        let mut stmts = self.nested_def(cx, generated);
        for (name, previous) in saved {
            match previous {
                Some(entry) => self.globals.insert(name, entry),
                None => self.globals.remove(&name),
            };
        }
        let Some(&local) = cx.scope.get(&mount_name) else { return error_expr(span) };
        let callee = mk(K::Local(local), cx.locals[local.0].ty, span);
        let mut call_args = vec![server];
        if function.is_none() {
            call_args.push(value);
        }
        call_args.push(prefix);
        stmts.push(hir::Stmt::Expr(mk(K::CallValue { callee: Box::new(callee), args: call_args }, Ty::NONE, span)));
        mk(K::Block { stmts, value: Box::new(mk(K::None, Ty::NONE, span)) }, Ty::NONE, span)
    }

    /// A name for `ty` usable in generated source: a temporary alias for structs and enums.
    fn alias_type(&mut self, ty: Ty, aliases: &mut Vec<(String, Global)>) -> String {
        match self.types.kind(ty).clone() {
            TyKind::Int => "int".to_string(),
            TyKind::Float => "float".to_string(),
            TyKind::Bool => "bool".to_string(),
            TyKind::Str => "str".to_string(),
            TyKind::None => "None".to_string(),
            TyKind::List(t) => format!("list[{}]", self.alias_type(t, aliases)),
            TyKind::Option(t) => format!("{}?", self.alias_type(t, aliases)),
            TyKind::Dict(k, v) => format!("dict[{}, {}]", self.alias_type(k, aliases), self.alias_type(v, aliases)),
            TyKind::Tuple(items) => {
                let items: Vec<String> = items.iter().map(|t| self.alias_type(*t, aliases)).collect();
                format!("tuple[{}]", items.join(", "))
            }
            TyKind::FrozenList(t) => format!("tuple[{}, ...]", self.alias_type(t, aliases)),
            TyKind::Struct(id) => {
                let name = format!("__g{}", aliases.len());
                aliases.push((name.clone(), Global::Struct(id)));
                name
            }
            TyKind::Enum(id) => {
                let name = format!("__g{}", aliases.len());
                aliases.push((name.clone(), Global::Enum(id)));
                name
            }
            _ => self.display(ty),
        }
    }
}

/// A function's docstring: its first statement, if that is a plain string.
/// Whether `text` is a rate such as `5/minute`, as `http.rate_limit` reads it.
fn valid_rate(text: &str) -> bool {
    let Some((count, period)) = text.split_once('/') else { return false };
    let count = count.trim();
    !count.is_empty()
        && count.bytes().all(|b| b.is_ascii_digit())
        && count.parse::<u64>().is_ok_and(|n| (1..1 << 31).contains(&n))
        && matches!(period.trim(), "second" | "minute" | "hour" | "day")
}

fn docstring(function: &ast::Function) -> Option<&str> {
    match &function.body.first()?.kind {
        ast::StmtKind::Expr(ast::Expr { kind: ast::ExprKind::Str(text), .. }) => Some(text),
        _ => None,
    }
}

/// A docstring cleaned as Python's `inspect.cleandoc` does (leading blank lines and the common
/// indentation of the lines after the first removed), then split into its first paragraph, joined
/// into one line, and the rest.
pub(super) fn split_docstring(text: &str) -> (String, String) {
    let lines: Vec<&str> = text.lines().collect();
    let indent = lines
        .iter()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.len() - line.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut cleaned: Vec<String> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index == 0 {
            cleaned.push(line.trim().to_string());
        } else if line.trim().is_empty() {
            cleaned.push(String::new());
        } else {
            cleaned.push(line.get(indent..).unwrap_or(line.trim_start()).trim_end().to_string());
        }
    }
    while cleaned.first().is_some_and(|l| l.is_empty()) {
        cleaned.remove(0);
    }
    while cleaned.last().is_some_and(|l| l.is_empty()) {
        cleaned.pop();
    }
    let blank = cleaned.iter().position(|l| l.is_empty()).unwrap_or(cleaned.len());
    let summary = cleaned[..blank].iter().map(|l| l.trim()).collect::<Vec<_>>().join(" ");
    let mut rest = &cleaned[blank..];
    while rest.first().is_some_and(|l| l.is_empty()) {
        rest = &rest[1..];
    }
    (summary, rest.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::split_docstring;

    #[test]
    fn docstrings_split_into_a_summary_and_a_description() {
        assert_eq!(split_docstring("Fetch one todo."), ("Fetch one todo.".into(), String::new()));
        assert_eq!(
            split_docstring("Fetch one todo.\n\n        Returns 404 when\n          it is missing.\n        "),
            ("Fetch one todo.".into(), "Returns 404 when\n  it is missing.".into())
        );
        assert_eq!(
            split_docstring("\n    A summary that runs\n    over two lines.\n\n\n    Body.\n    "),
            ("A summary that runs over two lines.".into(), "Body.".into())
        );
        assert_eq!(split_docstring("   "), (String::new(), String::new()));
    }
}
