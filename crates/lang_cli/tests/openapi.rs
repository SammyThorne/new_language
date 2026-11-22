//! The OpenAPI document of docs/12. `tests/openapi/schemas` prints the document of a program
//! with every kind of JSON type, then values of those types as the JSON encoder writes them:
//! each must fit its generated schema. The todo service in `tests/http/` serves its document,
//! which must say what the service really does: every status it declares is one the service
//! gives, and every body fits the declared schema. Both documents are compared with golden
//! files; set the `BLESS` environment variable (with the language's prefix) to rewrite them.

mod common;

use common::json::{self, Json};
use common::{Response, build_runtime, compiler, connect, read_response, repo_root, start};
use lang_names::{SOURCE_EXTENSION, env_var};
use std::collections::BTreeSet;
use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::process::Command;

/// Compare a document with a golden file, or rewrite the file when blessing.
fn golden(doc: &Json, file: &str) {
    let path = repo_root().join(file);
    if std::env::var_os(env_var("BLESS")).is_some() {
        std::fs::write(&path, doc.pretty()).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("{file} is missing; bless to write it"));
    let expected = json::parse(&expected).unwrap_or_else(|e| panic!("{file}: {e}"));
    assert!(*doc == expected, "the document differs from {file}:\n{}", doc.pretty());
}

fn schema_ref(name: &str) -> Json {
    Json::Obj(vec![("$ref".to_string(), Json::Str(format!("#/components/schemas/{name}")))])
}

#[test]
fn schemas_follow_the_json_encoding() {
    build_runtime();
    let output = Command::new(compiler())
        .args(["run", &format!("tests/openapi/schemas.{SOURCE_EXTENSION}")])
        .current_dir(repo_root())
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{}{}", stdout, String::from_utf8_lossy(&output.stderr));
    let mut lines = stdout.lines();
    let doc = json::parse(lines.next().unwrap()).unwrap();
    golden(&doc, "tests/openapi/schemas.openapi.json");

    let mut checked = BTreeSet::new();
    for line in lines {
        let (name, text) = line.split_once(' ').unwrap();
        let value = json::parse(text).unwrap_or_else(|e| panic!("{line}: {e}"));
        if let Err(problem) = json::validate(&doc, &schema_ref(name), &value) {
            panic!("a `{name}` the encoder wrote does not fit its schema: {problem}\n{text}");
        }
        checked.insert(name.to_string());
    }
    // Every documented schema had a value checked against it, apart from the error body.
    let documented: BTreeSet<String> =
        doc.at(&["components", "schemas"]).keys().into_iter().filter(|n| *n != "Error").map(String::from).collect();
    let unchecked: Vec<&String> = documented.difference(&checked).filter(|n| !n.starts_with("Page_")).collect();
    assert!(unchecked.is_empty(), "no sample value for {unchecked:?}");

    // The validator rejects what does not fit, so the checks above mean something.
    for (name, text) in [
        ("Color", r#""Blue""#),
        ("Shape", r#"{"type":"Rect","w":1}"#),
        ("Shape", r#"{"type":"Square","w":1,"h":1}"#),
        ("Node", r#"{"value":"one"}"#),
        ("Node", r#"{"children":[]}"#),
        ("Node", r#"{"value":1,"children":[{"value":1.5}]}"#),
        (
            "Everything",
            r#"{"small":1,"tiny":1,"big":1,"ratio":1,"display-name":"n","tags":["a","a"],"pair":[1,"x"],"frozen":[],"extra":{},"anything":null}"#,
        ),
        (
            "Everything",
            r#"{"small":1,"tiny":-1,"big":1,"ratio":1,"display-name":"n","tags":[],"pair":[1,"x"],"frozen":[],"extra":{},"anything":null}"#,
        ),
        (
            "Everything",
            r#"{"small":1,"tiny":1,"big":1,"ratio":1,"display-name":"n","tags":[],"pair":[1,"x",2],"frozen":[],"extra":{},"anything":null}"#,
        ),
        (
            "Everything",
            r#"{"small":1,"tiny":1,"big":1,"ratio":1,"display-name":"n","tags":[],"pair":[1,"x"],"frozen":[],"extra":{},"anything":null,"unknown":1}"#,
        ),
        (
            "Everything",
            r#"{"small":3000000000,"tiny":1,"big":1,"ratio":1,"display-name":"n","tags":[],"pair":[1,"x"],"frozen":[],"extra":{},"anything":null}"#,
        ),
    ] {
        let value = json::parse(text).unwrap();
        assert!(json::validate(&doc, &schema_ref(name), &value).is_err(), "{name} accepted {text}");
    }

    // What else the document says.
    let users = doc.at(&["paths", "/users/", "get"]);
    assert_eq!(users.at(&["operationId"]).as_str(), Some("Users_list"), "a method name two types share");
    assert_eq!(users.at(&["summary"]).as_str(), Some("List users."));
    let again = doc.at(&["paths", "/v2/again/raw", "post"]);
    assert_eq!(again.at(&["operationId"]).as_str(), Some("raw_2"), "a type mounted twice");
    assert_eq!(doc.at(&["paths", "/users/live", "get", "x-websocket"]), &Json::Bool(true));
}

fn request(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Response {
    request_with(stream, reader, method, path, body, &[])
}

fn request_with(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    method: &str,
    path: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> Response {
    let mut text = format!("{method} {path} HTTP/1.1\r\nHost: test\r\n");
    for (name, value) in headers {
        text.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        text.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    text.push_str("\r\n");
    text.push_str(body.unwrap_or(""));
    stream.write_all(text.as_bytes()).unwrap();
    read_response(reader, false)
}

/// Requests to the todo service: method, URL, the documented path it falls under, body, and
/// the status it gives.
const BEHAVIOUR: &[(&str, &str, &str, Option<&str>, u16)] = &[
    ("GET", "/health", "/health", None, 200),
    ("GET", "/api/todos", "/api/todos", None, 200),
    ("GET", "/api/todos?limit=x", "/api/todos", None, 400),
    ("POST", "/api/todos", "/api/todos", Some(r#"{"title": "write docs"}"#), 201),
    ("POST", "/api/todos", "/api/todos", Some(r#"{"titl": 1}"#), 400),
    ("POST", "/api/todos", "/api/todos", Some(r#"{"title": " "}"#), 422),
    ("GET", "/api/todos/1", "/api/todos/{id}", None, 200),
    ("GET", "/api/todos/abc", "/api/todos/{id}", None, 400),
    ("GET", "/api/todos/9", "/api/todos/{id}", None, 404),
    ("PATCH", "/api/todos/1", "/api/todos/{id}", Some(r#"{"done": true}"#), 200),
    ("PATCH", "/api/todos/1", "/api/todos/{id}", Some(r#"{"done": 3}"#), 400),
    ("PATCH", "/api/todos/9", "/api/todos/{id}", Some("{}"), 404),
    ("GET", "/api/todos", "/api/todos", None, 200),
    ("DELETE", "/api/todos/abc", "/api/todos/{id}", None, 400),
    ("DELETE", "/api/todos/1", "/api/todos/{id}", None, 204),
    ("GET", "/echo/a/b", "/echo/{rest}", None, 200),
];

#[test]
fn todo_service_documents_itself() {
    build_runtime();
    let program = "todo_annotations";
    let mut server = start(program);
    let (mut stream, mut reader) = connect(server.port);

    let served = request(&mut stream, &mut reader, "GET", "/docs/openapi.json", None);
    assert_eq!((served.status, served.header("content-type")), (200, Some("application/json")));
    let doc = json::parse(&served.body).unwrap_or_else(|e| panic!("{e}\n{}", served.body));
    golden(&doc, "tests/http/todo_annotations.openapi.json");
    assert!(doc.at(&["paths"]).keys().iter().all(|p| !p.starts_with("/docs")), "the docs routes are not listed");

    let yaml = request(&mut stream, &mut reader, "GET", "/docs/openapi.yaml", None);
    assert_eq!((yaml.status, yaml.header("content-type")), (200, Some("application/yaml")));
    let from_yaml = json::parse_yaml(&yaml.body).unwrap_or_else(|e| panic!("{e}\n{}", yaml.body));
    assert!(from_yaml == doc, "the YAML says something else:\n{}", yaml.body);

    // Every answer is one the document declares, with a body that fits.
    let mut seen = BTreeSet::new();
    for (method, url, path, body, status) in BEHAVIOUR {
        let response = request(&mut stream, &mut reader, method, url, *body);
        assert_eq!(response.status, *status, "{method} {url}: {}", response.body);
        let operation = doc.at(&["paths", path, &method.to_ascii_lowercase()]);
        let declared = operation
            .at(&["responses"])
            .get(&status.to_string())
            .unwrap_or_else(|| panic!("{method} {url} gave {status}, which the document does not list"));
        seen.insert((path.to_string(), method.to_string(), status.to_string()));
        match declared.get("content") {
            Some(content) => {
                let media = content.keys()[0];
                assert!(response.header("content-type").unwrap_or("").starts_with(media), "{method} {url}: type");
                if let Some(schema) = content.at(&[media]).get("schema")
                    && media == "application/json"
                {
                    let value = json::parse(&response.body).unwrap();
                    json::validate(&doc, schema, &value)
                        .unwrap_or_else(|e| panic!("{method} {url}: the body does not fit: {e}\n{}", response.body));
                }
            }
            None => assert!(*status == 204 || path.starts_with("/echo"), "{method} {url}: no content declared"),
        }
    }
    // ...and every status the document declares for the service's routes really happens.
    for (path, item) in match doc.at(&["paths"]) {
        Json::Obj(pairs) => pairs.clone(),
        _ => unreachable!(),
    } {
        if path == "/boom" {
            continue;
        }
        for method in item.keys() {
            for status in item.at(&[method, "responses"]).keys() {
                let key = (path.clone(), method.to_ascii_uppercase(), status.to_string());
                assert!(seen.contains(&key), "{} {path} declares {status}, which no request produced", key.1);
            }
        }
    }

    // The page and its files: each with its type, a validator, and the security headers on the
    // page; a browser that has the current version gets `304`.
    let page = request(&mut stream, &mut reader, "GET", "/docs", None);
    assert_eq!((page.status, page.header("content-type")), (200, Some("text/html; charset=utf-8")));
    let policy = page.header("content-security-policy").unwrap_or("");
    assert!(policy.contains("default-src 'self'") && !policy.contains("unsafe-inline"), "{policy}");
    assert_eq!(page.header("referrer-policy"), Some("no-referrer"));
    assert!(page.body.contains("<title>Todo API · API documentation</title>"), "{}", page.body);
    assert!(page.body.contains(r#"<meta name="openapi-url" content="/docs/openapi.json">"#));
    assert!(!page.body.contains("{{"), "a placeholder is left: {}", page.body);
    let slash = request(&mut stream, &mut reader, "GET", "/docs/", None);
    assert_eq!(slash.body, page.body);
    for (path, content_type) in [
        ("/docs", "text/html; charset=utf-8"),
        ("/docs/viewer.css", "text/css; charset=utf-8"),
        ("/docs/core.js", "text/javascript; charset=utf-8"),
        ("/docs/viewer.js", "text/javascript; charset=utf-8"),
    ] {
        let file = request(&mut stream, &mut reader, "GET", path, None);
        assert_eq!((file.status, file.header("content-type")), (200, Some(content_type)), "{path}");
        assert!(!file.body.is_empty(), "{path}");
        assert_eq!(file.header("cache-control"), Some("no-cache"), "{path}");
        assert_eq!(file.header("x-content-type-options"), Some("nosniff"), "{path}");
        let etag = file.header("etag").unwrap_or_else(|| panic!("{path} has no ETag")).to_string();
        assert!(etag.starts_with('"') && etag.ends_with('"'), "{path}: {etag}");
        let again = format!("GET {path} HTTP/1.1\r\nHost: test\r\nIf-None-Match: {etag}\r\n\r\n");
        stream.write_all(again.as_bytes()).unwrap();
        let cached = read_response(&mut reader, false);
        assert_eq!((cached.status, cached.body.as_str()), (304, ""), "{path}");
        assert_eq!(cached.header("etag"), Some(etag.as_str()), "{path}");
        let stale = format!("GET {path} HTTP/1.1\r\nHost: test\r\nIf-None-Match: \"1\"\r\n\r\n");
        stream.write_all(stale.as_bytes()).unwrap();
        assert_eq!(read_response(&mut reader, false).status, 200, "{path}");
    }
    server.stop(program);
}

#[test]
fn security_schemes_and_tags_match_the_service() {
    build_runtime();
    let program = "secured_docs";
    let mut server = start(program);
    let (mut stream, mut reader) = connect(server.port);
    let served = request(&mut stream, &mut reader, "GET", "/docs/openapi.json", None);
    let doc = json::parse(&served.body).unwrap_or_else(|e| panic!("{e}\n{}", served.body));
    golden(&doc, "tests/http/secured_docs.openapi.json");

    // The tags in the order given, then the schemes, and which operation needs which.
    let tags: Vec<&str> = match doc.at(&["tags"]) {
        Json::Arr(items) => items.iter().map(|t| t.at(&["name"]).as_str().unwrap()).collect(),
        other => panic!("tags: {other:?}"),
    };
    assert_eq!(tags, ["notes", "admin"]);
    assert_eq!(
        doc.at(&["components", "securitySchemes"]).keys(),
        ["googleSession", "key", "login", "session", "sid", "token"]
    );
    assert_eq!(doc.at(&["components", "securitySchemes", "googleSession", "name"]).as_str(), Some("session"));
    assert_eq!(doc.at(&["components", "securitySchemes", "sid", "in"]).as_str(), Some("cookie"));
    assert!(doc.at(&["paths", "/notes", "get"]).get("security").is_none(), "reading is open");
    let needs = |path: &str, method: &str| -> Vec<String> {
        match doc.at(&["paths", path, method, "security"]) {
            Json::Arr(any_of) => any_of.iter().flat_map(|r| r.keys().into_iter().map(String::from)).collect(),
            other => panic!("{method} {path}: {other:?}"),
        }
    };
    assert_eq!(needs("/notes", "post"), ["token"]);
    assert_eq!(needs("/report", "get"), ["key", "token"]);
    assert_eq!(needs("/me", "get"), ["session"], "an `auth.User` parameter protects the route");

    // The service does what the document says, with each status declared.
    let token = [("Authorization", "Bearer secret")];
    let key = [("X-API-Key", "k123")];
    let note = Some(r#"{"text": "hi"}"#);
    let cookie = request(&mut stream, &mut reader, "GET", "/test/cookie", None).body;
    let signed_in = [("Cookie", cookie.as_str())];
    type Headers<'a> = &'a [(&'a str, &'a str)];
    let cases: [(&str, &str, Option<&str>, Headers, u16); 8] = [
        ("GET", "/notes", None, &[], 200),
        ("POST", "/notes", note, &[], 401),
        ("POST", "/notes", note, &token, 201),
        ("GET", "/report", None, &[], 401),
        ("GET", "/report", None, &key, 200),
        ("GET", "/report", None, &token, 200),
        ("GET", "/me", None, &[], 401),
        ("GET", "/me", None, &signed_in, 200),
    ];
    for (method, path, body, headers, status) in cases {
        let response = request_with(&mut stream, &mut reader, method, path, body, headers);
        assert_eq!(response.status, status, "{method} {path} with {headers:?}: {}", response.body);
        let declared = doc.at(&["paths", path, &method.to_ascii_lowercase(), "responses"]);
        assert!(declared.get(&status.to_string()).is_some(), "{method} {path} gave {status}, which is not declared");
    }
    server.stop(program);
}
