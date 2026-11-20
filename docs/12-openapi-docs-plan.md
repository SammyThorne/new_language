# OpenAPI and a built-in API documentation page: research and plan

This is a plan, written so that an agent with no other context can carry it out. Nothing in it is built yet. It adds to the `http` module a function, `server.docs("/docs")`, that serves an OpenAPI document describing the server's routes and a ready-made, Swagger-style documentation page for it, at a path the user chooses. It mirrors what `@fastify/swagger` with `@fastify/swagger-ui` does in Fastify, and what FastAPI does at `/docs`. **The page is a viewer written for this project** (plain HTML, CSS and JavaScript, no dependencies, nothing downloaded), not a copy of Swagger UI; section 2 gives the reasons and section 4.5 the design.

Read first: [05-standard-library.md](05-standard-library.md) (HTTP, "Route annotations" and "How handler parameters are bound"), `crates/lang_sema/src/check/http.rs` (how routes are checked and how `mount` generates adapters), `stdlib/http.lang` (the `Server`), and the conventions in [11-release-polish-plan.md](11-release-polish-plan.md) (commit and push per milestone, the gates, the rules in section "Rules").

## 1. What the user gets

```python
import http

@derive(Json)
struct Todo:
    id: int
    title: str
    done: bool = False

impl App:
    @http.get("/todos/{id}", tags=["todos"], errors=[404])
    def get(self, id: int) -> Result[Todo]:
        """Fetch one todo.

        Returns 404 when there is no todo with that id.
        """
        ...

def main():
    server = http.Server()
    server.mount(App(...))
    server.docs("/docs", title="Todo API", version="1.0.0")   # the new function
    server.listen(":8080")
```

- `GET /docs` is the documentation page: the routes grouped by tag, each with its parameters, body, responses and schemas, and a "Try it out" form that sends a real request.
- `GET /docs/openapi.json` (and `/docs/openapi.yaml`) is the document, as Fastify serves `/documentation/json` and `/documentation/yaml`.
- The document is produced from the program's own types and annotations. Nothing is written twice: path and query parameters, the JSON body, the success status, the response type and the 400 that the compiler already generates come from the handler's signature.
- The page works with no internet connection: it is part of the runtime library, loads nothing from elsewhere, and needs no inline script or style, so it works under a strict Content-Security-Policy. A program that never calls `docs` does not contain it.

## 2. Research

**Fastify.** Two packages, split in version 8: `@fastify/swagger` only generates the specification (Swagger 2 by default, OpenAPI when an `openapi` option is given), from each route's `schema` option, and serves it as JSON and YAML; `@fastify/swagger-ui` serves the Swagger UI front end, by default under `routePrefix: '/documentation'`, with `uiConfig` passed to Swagger UI, `indexPrefix` for reverse proxies, and hooks on the documentation routes. A static mode serves a spec file the user wrote. In Fastify the user must write a JSON schema for every route; here the compiler already has the types, which is the main advantage of doing this in this language.

**FastAPI.** Generates the document at `/openapi.json`, Swagger UI at `/docs` and ReDoc at `/redoc`, from type hints. The UI's JavaScript and CSS come from a CDN (jsDelivr) by default; self-hosting is a documented how-to. Each URL can be set to `None` to turn it off.

**Swagger UI, and why this plan does not use it.** It is distributed as `swagger-ui-dist` on npm (Apache-2.0); the built files that matter are `swagger-ui-bundle.js` and `swagger-ui.css`, about 1.5 MB together (not measured here: measure before quoting). Using it means downloading a third-party bundle, committing it with its licence notices, and carrying it in every program that calls `docs`. The owner chose to build a viewer instead: no download, no third-party code to track for security fixes, a few tens of kilobytes instead of megabytes, and a page that can be made to match the rest of the project. The cost is that this project writes and maintains a small front end; section 4.5 keeps its scope to what a backend developer uses (read the operations, try them), and section 7 lists what it will not do.

What the viewer must match, taken from what Swagger UI, FastAPI's page and Fastify's page give a user: operations grouped by tag, collapsible, with the method and path, summary and description; a parameters table (name, `in`, type, required, default); request body and response schemas shown as a tree, with an example generated from the schema; a "Try it out" form that fills in parameters and a JSON body and shows the status, headers, body and the equivalent `curl` command; a list of the schemas ("Models"); a filter box; and an "Authorize" box for bearer and API-key schemes.

**Decision: the spec is OpenAPI 3.1.0.** It is what current tools read, JSON Schema 2020-12 is what 3.1 uses (a nullable value is `{"type": ["string", "null"]}`), and Swagger 2 is obsolete. Tools that only read 3.0 are a known cost; an option to emit 3.0.3 is not planned (see section 7).

## 3. What the compiler and library already have

Facts found by reading the code, which the design relies on:

- `check/http.rs` records a `Route` for every annotated handler (`method`, `path`, `status`, `params: Vec<Binding>`, `reply`, `fallible`, `ws`). `Binding` says where each parameter comes from: `Path{name, kind}`, `Query{name, kind, optional, default}`, `Header`, `Cookie`, `Body(Ty)`, `Request`, `Socket`. `Reply` is `Response | Json | Text | Empty`. This is nearly the whole OpenAPI operation.
- `mount` already generates Weby source for each route (`__server.route(method, __prefix + path, __route_i)`), checks it as user code and runs it. Documentation metadata can be generated the same way, with the prefix added at run time.
- Types that derive `Json` (`check/json.rs`, `@json(name=...)`, defaults, `T?`, `list`, `dict[str, V]`, tuples, enums tagged or as strings, `json.Value`) have a fixed wire form, described in docs/05 "JSON". The schema generator must follow `check/json.rs` and the codegen in `lang_codegen/src/json.rs`, not the docs, where they differ.
- **Function docstrings are parsed and thrown away.** A function's docstring is its first statement, a string expression; the parser skips module and struct docstrings (`parser.rs`, around lines 281 and 586). Struct docstrings are not kept in the AST.
- **Annotation arguments are only `Name`, `Str` and `Int`** (`ast::AnnotationValue`). Lists (`tags=["todos"]`, `errors=[404]`) and booleans need adding; `True` and `False` currently arrive as `Name`.
- Call-style routes (`server.get("/x", handler)`) are `def(Request) -> Result[Response]`: nothing is known about them except method and path (and `{name}` segments).
- `Server` stores `routes: list[Route]` in `stdlib/http.lang`. The compiler embeds the standard library with `include_str!` (`lang_sema/src/stdlib.rs`); the runtime (`lang_rt`) is a static archive linked with `--gc-sections`, so an unreferenced static in it is dropped from a program's executable (this is how executables became small in D95).
- `Response.body` is a `str`; `respond_raw(handle, status, content_type, headers, body)` writes it. The viewer's files are text, so they can be served as strings.
- `node` (20 here, preinstalled on GitHub's runners) can run the viewer's logic tests with `node --test`, which needs no package. The built-in browser pane can open a page for a real check. `chromium` and `firefox` are installed here too.

## 4. Design

### 4.1 Public API (all in `http`)

```python
server.docs(
    path: str = "/docs",            # where the UI is mounted; the document is at path + "/openapi.json"
    title: str = "API",
    version: str = "0.1.0",
    description: str = "",
    servers: list[str] = [],        # "servers" entries
    tags: list[http.Tag] = [],      # descriptions for tags (name, description)
    security: dict[str, http.SecurityScheme] = {},   # milestone 5
    ui: bool = True,                # False: serve only the JSON and YAML
    try_it_out: bool = True,        # False: the page is read-only, with no request form
    hide_ws: bool = True,
)

group.docs(...)                     # not planned: docs describe the server (see 4.6)
server.openapi_json(title=..., version=..., ...) -> str    # the document as text, for tests, files, or a custom route
```

- `docs` registers ordinary routes on the server (so middleware, including authentication, applies to them; the page says so in its docs, as Fastify's `uiHooks` exist for the same reason):
  - `GET path` -> the HTML page. `GET path/` serves the same page (no redirect needed, the files are linked with absolute paths built from `path`).
  - `GET path/openapi.json`, `GET path/openapi.yaml`.
  - `GET path/viewer.css`, `GET path/core.js` and `GET path/viewer.js`: the viewer's files (section 4.5). The page tells the script where the document is, and whether "Try it out" is on, with `<meta name="openapi-url">` and `<meta name="try-it-out">` tags (HTML-escaped), not an inline script.
- The document is built **on the first request**, not when `docs` is called, so `docs` can come before or after `mount` and group routes; it is cached afterwards. (If routes are registered after the first request, they are missing from the cache: documented, and `docs` registers an invalidation through `Server.chain = None`'s existing path.)
- The docs routes themselves are never listed in the document.

### 4.2 Where each part of the operation comes from

| OpenAPI | From |
|---|---|
| `paths[path][method]`, `operationId` | the route; `operationId` is the method or function name (made unique with the type name when two types have the same method name) |
| `summary`, `description` | the handler's docstring: the first line is `summary`, the rest (Markdown, dedented) is `description` |
| `tags` | `tags=[...]` on the annotation; default: the mounted type's name for methods, none for functions |
| `deprecated` | `deprecated=True` on the annotation |
| `parameters`, `in: path` | `Binding::Path`: `required: true`, schema from `Scalar` (`integer`/`number`/`boolean`/`string`) |
| `parameters`, `in: query` | `Binding::Query`: `required` unless optional or defaulted; `default` from the literal default; schema from `Scalar` |
| `parameters`, `in: header` / `cookie` | `Binding::Header`/`Cookie` (`user_agent` -> `User-Agent`), required unless `T?` |
| `requestBody` | `Binding::Body(ty)`: `application/json`, `required: true`, `$ref` to the schema |
| success response | `status=` or 200 (204 when `Reply::Empty`); `Reply::Json` -> `application/json` with the type's schema; `Reply::Text` -> `text/plain` string; `Reply::Empty` -> no content; `Reply::Response` -> description only (the handler builds the response, nothing is known) |
| `400` | added when anything bound can fail (a non-`str` path parameter, a required query/header/cookie, a body): an `Error` schema, `{"error": string}` (docs/05 "Error mapping") |
| other error statuses | `errors=[404, 409]` on the annotation: each becomes a response with the same `Error` schema and the standard reason phrase as description |
| WebSocket routes (`@http.ws`) | left out (OpenAPI cannot describe them); a `hide_ws=False` option lists them as `GET` with `101` and `x-websocket: true` |
| call-style routes | listed with method, path and `{name}` path parameters as strings, `summary` empty, one `default` response; an optional documentation argument is not part of this plan's first version (section 7) |

### 4.3 Schemas

`@derive(Json)` types become entries under `components.schemas` and are referenced by `$ref`, which also handles recursive types. The mapping follows the JSON encoding exactly:

| Weby | JSON Schema |
|---|---|
| `int` and sized integers | `{"type": "integer", "format": "int64"}` (`int32` for `i32`/`u32` where that is the width; see `lang_codegen/src/json.rs`) |
| `float` | `{"type": "number", "format": "double"}` |
| `bool`, `str` | `boolean`, `string` |
| `T?` | `{"type": [<T's type>, "null"]}`, or `oneOf` with `{"type": "null"}` for a `$ref` |
| `list[T]`, `tuple[T, ...]` | `array` with `items` |
| tuples | `array` with `prefixItems`, `minItems`/`maxItems` |
| `dict[str, V]` | `object` with `additionalProperties` |
| `set[T]` | `array` with `items` and `uniqueItems: true` (D93: sets are JSON arrays) |
| struct | `object`, `properties` using `@json(name=...)`, `required` = fields with no default and not optional, `additionalProperties: false` under `@json(strict)` |
| field with a literal default | `default` |
| fieldless enum | `{"type": "string", "enum": [...]}` |
| enum with fields | `oneOf` of one object per variant, each with a `type` property fixed to the variant name (the tagged form), discriminator `type` |
| `json.Value` | `{}` (any) |
| `Mutex`, functions, and other types that are not `Json` | not possible: the route checker already refuses them |

Component names are the type's name; a type from a module other than the root is `module.Name`; an instance of a generic type is `Name_Arg` (names must match `^[a-zA-Z0-9._-]+$`). Two different types that end up with the same name are an error in the compiler, reporting both (new code, section 6, milestone 2).

### 4.4 Where the work happens: compile time, only when used

- The compiler builds, per `mount`, a list of operation records and the schemas they reach, **as JSON text constants in the generated adapter source**, and registers them on the server with a private method (`__server.__document(method, __prefix + path, operation_json)`, `__server.__schemas(schemas_json)`). `Server` keeps them in two new private fields. Nothing is parsed at run time: the document is built by joining text and escaping the few user-supplied strings (title, description, servers) with the `json` module.
- **A program that does not use `docs` pays nothing.** The compiler emits the documentation calls only when the program mentions `docs` or `openapi_json` as a method name anywhere (a token scan like `check/unused.rs`; it can only be too generous, never too strict). The benchmark servers do not, so their code, counts and executable sizes must not change: a gate (section 6).
- The viewer's files live in `lang_rt` as `static` strings (`include_str!`), reached only through a new extern used by the `docs` code in `http.lang`; the linker drops them from programs that do not call `docs`.

### 4.5 The viewer

**Files.** Source in `crates/lang_rt/assets/viewer/`: `index.html` (a template with a few `{{title}}`-style placeholders that `docs` fills in with HTML-escaped values), `viewer.css`, `core.js` (logic with no DOM: testable under `node`) and `viewer.js` (the DOM code). Plain ES2020 JavaScript, no framework, no build step, no minifier, no external request. Target: under 60 KB in total, unminified.

**`core.js`** (also loadable by `node` with `module.exports`; every function below is tested without a browser):
- `resolve(doc, ref)`: follows `#/components/...` references, with a guard against cycles;
- `example(doc, schema)`: an example value from a schema (`default` if present, else by type: `0`, `0.0`, `""`, `false`, `[]` with one item, objects with their `required` properties first, `oneOf` takes the first branch, a recursive type stops after a depth limit);
- `typeLabel(doc, schema)`: `integer`, `string?`, `array<Todo>`, `object`, for the tables;
- `operations(doc)`: the flat list of operations grouped by tag, in the document's order, with parameters merged from path level and operation level;
- `buildRequest(doc, op, values, baseUrl)`: the URL (path parameters substituted and percent-encoded, query parameters encoded, optional ones left out when empty), the headers and the body, ready for `fetch`;
- `curl(request)`: the same request as a shell command, with single quotes escaped;
- `markdown(text)`: a tiny safe subset for descriptions (paragraphs, `code`, fenced blocks, `**bold**`, links with `http`, `https` or relative targets only), producing HTML by escaping everything first.

**`viewer.js`** builds the page from the document with DOM calls, never `innerHTML` with unescaped data (`markdown` is the single place that produces HTML, from escaped text). Features: header with title, version and description; a tag-grouped list of collapsible operations, with a coloured method badge; a filter box (matches method, path, summary, tag); for each operation the parameters table, the request body schema, the responses with their schemas, a deprecated marker; "Try it out" (inputs for each parameter, a JSON text area for the body prefilled from `example`, a Send button that uses `fetch` against the document's first `servers` entry or the page's own origin, and shows status, duration, response headers, the pretty-printed body, and the `curl` command with a copy button); a "Models" section; an "Authorize" box for the `bearer`, `apiKey` and `basic` schemes of milestone 5 (the value is kept in memory only, never in storage); the URL fragment opens the operation it names, so a link to one operation works. Light and dark follow `prefers-color-scheme`; the layout works at phone width; controls are real buttons and labelled inputs so the keyboard and screen readers work.

**Serving.** Each file is a route with `Content-Type` (with `charset=utf-8`), `ETag` (a hash of the content computed once) and `Cache-Control: no-cache` (revalidate, so an upgraded server is never stuck with an old page), and `If-None-Match` answered with `304`. The page also sets `Content-Security-Policy: default-src 'self'; connect-src *; img-src 'self' data:` (the browser has to reach the API at whatever `servers` says), `X-Content-Type-Options: nosniff` and `Referrer-Policy: no-referrer`. The files go out as plain strings; with files this small, compression is not worth a dependency.

### 4.6 Out of scope

`group.docs` (the document describes one server); routes added after the first request; request/response examples (`example=`) beyond defaults; callbacks, webhooks, links; OAuth2 and OpenID Connect flows in the viewer (a bearer token pasted into "Authorize" works), file uploads and `multipart/form-data` or XML bodies in "Try it out" (they are shown, not sendable), XML and YAML response rendering beyond text; ReDoc or Scalar as extra UIs (the document is standard, so a user can serve one themselves with `openapi_json` and `http.html`); a `weby openapi` command that writes the file without running the program (it needs the route table at compile time, which `mount` has but the CLI does not collect; a possible follow-up).

## 5. Owner decisions

Settled: **the viewer is written in this repository, and nothing is downloaded** (the owner's decision, replacing the earlier plan to vendor Swagger UI). That means no licence notices to carry, no checksums to record, and no approval step in the milestones.

Still open, with the plan's choice:

1. **The default path:** `/docs` (FastAPI's) or `/documentation` (Fastify's). This plan uses `/docs`.
2. **The name in the page and the docs.** The page is called "API documentation"; the plan avoids calling it Swagger UI, since it is not. Say "Swagger-style" at most.
3. **A next version number** for this feature (1.1.0 would fit); not changed without asking.

## 6. Milestones

Each milestone ends with its tests, the gates below, a "Done" note in this file, a decision-log entry (D96 and following in [08](08-decisions-and-risks.md)), a commit and a push to `main`.

**Gates for every milestone:** `cargo fmt --check`, `cargo clippy --workspace --all-targets --offline -- -D warnings`, `cargo test --workspace --offline`, `sh tests/stress.sh 300`. Because the HTTP server is on the performance-sensitive path, also `python3 bench/run.py --check` and `python3 bench/http/counts.py --check` (never concurrently with a build or tests). The benchmark servers must be unchanged.

### Milestone 1: annotations and docstrings (about 1 day)

- `ast::AnnotationValue` gains `List(Vec<AnnotationValue>)` and `Bool(bool)`; the parser, `fmt.rs` (it keeps tokens), the LSP and the editor grammars need no more than accepting them.
- `check/http.rs` `route_of` accepts `tags=[str, ...]`, `deprecated=True|False`, `errors=[int, ...]` (each 400 to 599) and keeps them on `Route`; a wrong value is a `BAD_ROUTE` error with a help line, like the existing `status=` check. Update the annotation table in docs/05.
- `Route` gets `summary` and `description`, taken from the handler's docstring (first statement of `function.body`, if it is a string expression; dedented as Python's `inspect.cleandoc` does).
- Tests: `tests/ui/route_annotation_errors.wb` (golden), a unit test of the docstring split. Existing programs must compile and run unchanged.

**Done** (D96). Annotation values can be `True`, `False` and lists; `tags=`, `deprecated=` and `errors=` are checked and kept on the route with the docstring's summary and description; `summary=` gets a help line pointing at the docstring. All gates passed, the benchmark checks included.

### Milestone 2: schemas and operations (about 2 days)

- New `crates/lang_sema/src/check/openapi.rs`: from a `Route` and the checker's type tables, produce the operation JSON and the component schemas (section 4.2 and 4.3), as text. Reuse the JSON-capability walk in `check/json.rs` so the schema and the encoder cannot disagree; add a test that, for every `@derive(Json)` type in `tests/`, encodes a sample value and checks it against the generated schema in a small validator in the test (jsonschema is not installed and cannot be downloaded: write the subset validator, about 150 lines, covering `type`, `properties`, `required`, `items`, `prefixItems`, `additionalProperties`, `enum`, `oneOf`, `$ref`).
- Name collisions between different types become a new error code (the next free one, `E0341` at the time of writing; add it to `lang_diag/src/codes.rs` and a section to `docs/errors.md`, which a test checks).
- `mount` emits the registration calls only when the program uses `docs` (the token scan). `Server` gets the two private fields and the private methods in `http.lang`.
- Tests: golden JSON for `tests/http/todo_annotations.wb` (`tests/http/todo_annotations.openapi.json`), a program with every schema form in the table, generics, a recursive type, a mounted prefix, two types with a method of the same name. Run the compiler with a debug flag or a test hook that prints the generated documentation source (the same way the mount adapters can be inspected today).
- Gate: the benchmark servers' generated code is byte-identical to before (compare `--emit=llvm-ir` or the object size).

### Milestone 3: `docs`, `openapi_json`, the document (about 1.5 days)

- `Server.docs(...)` and `Server.openapi_json(...)` in `http.lang`: build the document from the stored fragments (section 4.1), cached; JSON as above; YAML from a small emitter in the library working on the same text (quote strings; the document only has objects, arrays, strings, numbers, booleans and null). Call-style routes are included with their path parameters.
- The `docs` routes: `openapi.json` and `openapi.yaml`. The page and its files are milestone 4, so until then `docs` serves the document only.
- `title`, `description` and `servers` are JSON-escaped in the document.
- Tests: `crates/lang_cli/tests/http.rs` gains a case that starts the todo service with `docs`, fetches `/docs/openapi.json`, parses it, and checks the paths, parameters, schemas and statuses against the service's real behaviour (e.g. the declared 404 really occurs, the declared 400 really occurs); `/docs/openapi.yaml` parses to the same content (compare through a small YAML subset reader in the test, or by round trip with Python if available in the test environment: the tests may not require Python). A `tests/run/` program using `openapi_json` with an in-process `http.test_request`.

### Milestone 4: the viewer (about 4 days)

1. **`core.js` first, with its tests.** `crates/lang_rt/assets/viewer/core.js` and `crates/lang_rt/assets/viewer/core.test.js` (`node --test`, no packages). Cover every function in section 4.5 with the cases that go wrong: `$ref` cycles, a recursive schema, `oneOf`, nullable types (`["string","null"]`), path parameters with characters that need encoding, an empty optional query parameter, a body of each kind, `curl` quoting of a value with a single quote, and `markdown` given hostile input (`<script>`, `javascript:` links, unbalanced backticks). Add a Rust test in `crates/lang_cli/tests/` that runs `node --test` on the directory when `node` is on the PATH and prints a skip notice when it is not (CI has it).
2. **`viewer.js`, `viewer.css`, `index.html`.** Build the page from section 4.5, feature by feature, checking each in the built-in browser against the real todo service: the filter, the expand and collapse, a generated example, a successful "Try it out" call, the 400 and 404 cases (shown with their JSON bodies), the models list, the dark scheme (the browser pane can emulate it), and a phone-width layout.
3. **Serving.** `lang_rt` exposes the files as strings with their hashes; `docs` registers the routes of section 4.1 and the headers of section 4.5.
4. **Tests.** HTTP tests: each file is served with the right type and headers, `If-None-Match` gives `304`, the page escapes a title containing `<script>` and `"`, and the response carries the CSP. `crates/lang_cli/tests/executables.rs` gains two size checks: a program that does not call `docs` is not larger than before (compare hello and the benchmark server with the sizes in `bench/README.md`), and one that does is larger by less than 100 KB.
5. **A browser check that is kept.** Write `tests/http/docs_page_check.md` as a short manual script (the steps above and what to see) and run it once for real before the commit, with screenshots in the notes in this file. If a headless Chromium can be driven offline with what is installed, add it to the Rust tests behind an environment variable; it is not required, and CI does not run it.
6. Measure and write down: the viewer's size, the executable's growth, and that a request to the docs routes allocates and costs what a request to a small static route does.

### Milestone 5: polish and close-out (about 1 day)

- `security`: `http.SecurityScheme` (bearer, API key in a header or cookie, basic) listed in `components.securitySchemes`, and `security=["name"]` on an annotation to mark an operation (the viewer's "Authorize" box uses this: it adds the token or key to every "Try it out" request).
- `tags=[http.Tag(...)]` descriptions and ordering.
- Documentation: a new "OpenAPI and the documentation page" subsection in docs/05 and in `examples.md` (the program in section 1, compiled by the guide test), the API table, the error code, `docs/release-notes.md` ("What changed" and limitations), `docs/06-roadmap.md`, README feature list, `editors/` snippets if they list annotation names, and the D entries.
- Run the whole gate list, including the benchmark checks, and the browser script of milestone 4 again. Update this file with what was built and what was left out.
- Leave to the owner: whether the next version number is 1.1.0 (this is a new feature; do not change the version without asking).

## 7. Risks and open questions

- **A front end to maintain.** The viewer is code with its own bugs and browser differences. Mitigations: no dependencies, plain DOM calls, the logic split out and tested under `node`, and a deliberately short list of features (section 4.6). Where it falls short the document is standard, so users can point Swagger UI, Redoc or Scalar at `/docs/openapi.json` themselves.
- **Not Swagger UI.** People who know Swagger UI will find the page familiar but not identical; some features (OAuth2 flows, uploads) are missing. Say so in the documentation.
- **Cross-site scripting.** The document contains text from the program's authors (docstrings, titles) and from nowhere else, but the viewer still treats all of it as untrusted: everything is escaped, `markdown` allows no raw HTML and only safe link schemes, the CSP forbids inline script, and the tests include hostile input. "Try it out" sends requests with the viewer's page origin, so on a shared origin it could be used to call the API as the logged-in user; the page is as sensitive as the API (documented, with the advice to protect `/docs` with the server's middleware in production, or turn it off).
- **The spec follows the encoder.** If the JSON encoding of a type changes later, the schema must change with it; the milestone 2 test that validates encoded samples against the generated schema is what keeps them together. Keep it in the standard test run.
- **Errors from `Result` handlers are not typed.** `Result[T]` carries any `Error`, so which statuses a handler can return is unknown to the compiler; `errors=[...]` is the user's statement, and nothing checks it. (A later improvement could scan for `http.Error(404, ...)` literals, which is fragile; not planned.)
- **Call-style routes** are documented thinly. The cost of richer metadata is an extra argument on every registration method; deferred until there is demand. Annotated routes are the recommended style and are fully described.
- **OpenAPI 3.0.** Some code generators and gateways read only 3.0. A converter (nullable types, `prefixItems`) is a contained follow-up; not part of this plan.
- **Docs behind authentication.** The docs routes pass through the server's middleware, so an authentication middleware blocks the page along with the API; the browser cannot send a bearer token for the page itself. Document the pattern (let the middleware skip the docs path, or require its own check on it) in the example.
- **The checker's pre-scan for `docs`** must also cover programs that call it through a module alias or a wrapper function: it looks for the identifier `docs`/`openapi_json` after a `.` anywhere, so it is conservative; a user-defined method also called `docs` only costs a little extra metadata.

## 8. Appendix: what the document looks like

For the todo service of docs/05 (`App` with `list`, `create`, `get`, `remove`) mounted at `/` and `docs("/docs", title="Todo API", version="1.0.0")`, `GET /docs/openapi.json` should contain, abbreviated:

```json
{
  "openapi": "3.1.0",
  "info": {"title": "Todo API", "version": "1.0.0"},
  "paths": {
    "/todos": {
      "get": {
        "operationId": "list", "tags": ["App"],
        "parameters": [{"name": "limit", "in": "query", "required": false,
                        "schema": {"type": "integer", "format": "int64", "default": 50}}],
        "responses": {
          "200": {"description": "OK", "content": {"application/json": {
            "schema": {"type": "array", "items": {"$ref": "#/components/schemas/Todo"}}}}},
          "400": {"description": "Bad Request", "content": {"application/json": {
            "schema": {"$ref": "#/components/schemas/Error"}}}}
        }
      },
      "post": {
        "operationId": "create", "tags": ["App"],
        "requestBody": {"required": true, "content": {"application/json": {
          "schema": {"$ref": "#/components/schemas/NewTodo"}}}},
        "responses": {"201": {"description": "Created", "content": {"application/json": {
          "schema": {"$ref": "#/components/schemas/Todo"}}}},
                      "400": {"$ref": "#/components/responses/BadRequest"}}
      }
    },
    "/todos/{id}": {
      "get": {
        "operationId": "get", "summary": "Fetch one todo.", "tags": ["todos"],
        "parameters": [{"name": "id", "in": "path", "required": true,
                        "schema": {"type": "integer", "format": "int64"}}],
        "responses": {"200": {"...": "Todo"}, "400": {"...": "Error"}, "404": {"...": "Error"}}
      },
      "delete": {"operationId": "remove", "responses": {"204": {"description": "No Content"}}}
    }
  },
  "components": {"schemas": {
    "Todo": {"type": "object", "required": ["id", "title"], "properties": {
      "id": {"type": "integer", "format": "int64"}, "title": {"type": "string"},
      "done": {"type": "boolean", "default": false}}},
    "NewTodo": {"type": "object", "required": ["title"], "properties": {"title": {"type": "string"}}},
    "Error": {"type": "object", "required": ["error"], "properties": {"error": {"type": "string"}}}
  }}
}
```

(Whether repeated responses use `components.responses` and `$ref`, as sketched for `400` in `create`, or are written out each time is the implementer's choice; the content is what matters.)

Sources for the research: the `@fastify/swagger` and `@fastify/swagger-ui` READMEs and migration guide on npm and unpkg; FastAPI's "Custom Docs UI Static Assets" how-to; the `swagger-ui-dist` package page on npm (the feature list the viewer is measured against).
