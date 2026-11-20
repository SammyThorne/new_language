# OpenAPI and a built-in Swagger UI: research and plan

This is a plan, written so that an agent with no other context can carry it out. Nothing in it is built yet. It adds to the `http` module a function, `server.docs("/docs")`, that serves an OpenAPI document describing the server's routes and a ready-made Swagger UI page for it, at a path the user chooses. It mirrors what `@fastify/swagger` with `@fastify/swagger-ui` does in Fastify, and what FastAPI does at `/docs`.

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

- `GET /docs` is the Swagger UI page.
- `GET /docs/openapi.json` (and `/docs/openapi.yaml`) is the document, as Fastify serves `/documentation/json` and `/documentation/yaml`.
- The document is produced from the program's own types and annotations. Nothing is written twice: path and query parameters, the JSON body, the success status, the response type and the 400 that the compiler already generates come from the handler's signature.
- The page works with no internet connection: Swagger UI is part of the runtime library, not loaded from a CDN. A program that never calls `docs` does not contain it.

## 2. Research

**Fastify.** Two packages, split in version 8: `@fastify/swagger` only generates the specification (Swagger 2 by default, OpenAPI when an `openapi` option is given), from each route's `schema` option, and serves it as JSON and YAML; `@fastify/swagger-ui` serves the Swagger UI front end, by default under `routePrefix: '/documentation'`, with `uiConfig` passed to Swagger UI, `indexPrefix` for reverse proxies, and hooks on the documentation routes. A static mode serves a spec file the user wrote. In Fastify the user must write a JSON schema for every route; here the compiler already has the types, which is the main advantage of doing this in this language.

**FastAPI.** Generates the document at `/openapi.json`, Swagger UI at `/docs` and ReDoc at `/redoc`, from type hints. The UI's JavaScript and CSS come from a CDN (jsDelivr) by default; self-hosting is a documented how-to. Each URL can be set to `None` to turn it off.

**Swagger UI.** Distributed as `swagger-ui-dist` on npm (the built files without the npm dependency tree), Apache-2.0 licensed. Version 5 reads OpenAPI 3.1. The files needed are `swagger-ui-bundle.js`, `swagger-ui-standalone-preset.js` (only for the top bar; may be left out) and `swagger-ui.css`. The package is large unpacked (11 MB with source maps and extra variants); the three files are about 1.5 MB together, and about a third of that compressed. **Measure these before deciding anything that depends on them.** The page is started by a small script that calls `SwaggerUIBundle({url, dom_id, ...})`; serving that as its own file, as Fastify does (`swagger-initializer.js`), keeps the page free of inline script, so it works under a strict Content-Security-Policy.

**Decision: the spec is OpenAPI 3.1.0.** Swagger UI 5 renders it, JSON Schema 2020-12 is what 3.1 uses (a nullable value is `{"type": ["string", "null"]}`), and Swagger 2 is obsolete. Tools that only read 3.0 are a known cost; an option to emit 3.0.3 is not planned (see section 7).

## 3. What the compiler and library already have

Facts found by reading the code, which the design relies on:

- `check/http.rs` records a `Route` for every annotated handler (`method`, `path`, `status`, `params: Vec<Binding>`, `reply`, `fallible`, `ws`). `Binding` says where each parameter comes from: `Path{name, kind}`, `Query{name, kind, optional, default}`, `Header`, `Cookie`, `Body(Ty)`, `Request`, `Socket`. `Reply` is `Response | Json | Text | Empty`. This is nearly the whole OpenAPI operation.
- `mount` already generates Weby source for each route (`__server.route(method, __prefix + path, __route_i)`), checks it as user code and runs it. Documentation metadata can be generated the same way, with the prefix added at run time.
- Types that derive `Json` (`check/json.rs`, `@json(name=...)`, defaults, `T?`, `list`, `dict[str, V]`, tuples, enums tagged or as strings, `json.Value`) have a fixed wire form, described in docs/05 "JSON". The schema generator must follow `check/json.rs` and the codegen in `lang_codegen/src/json.rs`, not the docs, where they differ.
- **Function docstrings are parsed and thrown away.** A function's docstring is its first statement, a string expression; the parser skips module and struct docstrings (`parser.rs`, around lines 281 and 586). Struct docstrings are not kept in the AST.
- **Annotation arguments are only `Name`, `Str` and `Int`** (`ast::AnnotationValue`). Lists (`tags=["todos"]`, `errors=[404]`) and booleans need adding; `True` and `False` currently arrive as `Name`.
- Call-style routes (`server.get("/x", handler)`) are `def(Request) -> Result[Response]`: nothing is known about them except method and path (and `{name}` segments).
- `Server` stores `routes: list[Route]` in `stdlib/http.lang`. The compiler embeds the standard library with `include_str!` (`lang_sema/src/stdlib.rs`); the runtime (`lang_rt`) is a static archive linked with `--gc-sections`, so an unreferenced static in it is dropped from a program's executable (this is how executables became small in D95).
- `Response.body` is a `str`; `respond_raw(handle, status, content_type, headers, body)` writes it. Strings are length-delimited bytes, so a gzip body can travel in one, provided the runtime does not validate UTF-8 on this path (check).

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
    cdn: bool = False,              # True: the page loads Swagger UI from jsDelivr instead of the built-in copy
    hide_ws: bool = True,
)

group.docs(...)                     # not planned: docs describe the server (see 4.6)
server.openapi_json(title=..., version=..., ...) -> str    # the document as text, for tests, files, or a custom route
```

- `docs` registers ordinary routes on the server (so middleware, including authentication, applies to them; the page says so in its docs, as Fastify's `uiHooks` exist for the same reason):
  - `GET path` -> the HTML page. `GET path/` serves the same page (no redirect needed, assets use absolute URLs built from `path`).
  - `GET path/openapi.json`, `GET path/openapi.yaml`.
  - `GET path/static/swagger-ui.css`, `.../swagger-ui-bundle.js`, `.../swagger-initializer.js` (generated: sets `url` to the document and Swagger UI options).
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
- The Swagger UI files live in `lang_rt` as `static` byte arrays, reached only through a new extern used by the `docs` code in `http.lang`; the linker drops them from programs that do not call `docs`.

### 4.5 Serving the UI files

- Stored **gzip-compressed** in the repository and the runtime (`include_bytes!`), served with `Content-Encoding: gzip`, `Content-Type`, `Cache-Control: public, max-age=31536000, immutable` and an `ETag` (the SHA-256 of the file), answering `If-None-Match` with `304`. A client that does not send `Accept-Encoding: gzip` gets `406`; every browser sends it, and an inflate implementation is not worth adding for it.
- The body goes to the connection from static memory without copying: a new runtime function returns a `str` that points at the static bytes (Boehm ignores pointers outside its heap).
- `cdn=True` skips all of this: the page points at `https://cdn.jsdelivr.net/npm/swagger-ui-dist@<pinned version>/...`, for programs that want a smaller executable and have internet.

### 4.6 Out of scope

`group.docs` (the document describes one server); routes added after the first request; request/response examples (`example=`) beyond defaults; callbacks, webhooks, links; ReDoc or Scalar as extra UIs (the document is standard, so a user can serve one themselves with `openapi_json` and `http.html`); a `weby openapi` command that writes the file without running the program (it needs the route table at compile time, which `mount` has but the CLI does not collect; a possible follow-up).

## 5. Owner decisions and approvals needed before building

1. **Downloading Swagger UI.** The built files come from the npm package `swagger-ui-dist` (registry.npmjs.org). Downloading is not done without the owner's approval: ask, naming the package, the exact version, and the files. Record the SHA-256 of the tarball and of each file in `crates/lang_rt/assets/README.md`, with the upstream `LICENSE` and `NOTICE` (Apache-2.0 requires them to travel with the files), and the version and checksums in a decision-log entry. Without the approval, build milestones 1 to 3 and the `cdn=True` mode; milestone 4 then ships the page without the built-in copy.
2. **Adding about 0.5 MB (compressed) to the repository and to programs that use `docs`.** Measure after vendoring; if the owner finds it too much, `cdn=True` can become the default.
3. **The default path** (`/docs` as FastAPI, or `/documentation` as Fastify). This plan uses `/docs`.

## 6. Milestones

Each milestone ends with its tests, the gates below, a "Done" note in this file, a decision-log entry (D96 and following in [08](08-decisions-and-risks.md)), a commit and a push to `main`.

**Gates for every milestone:** `cargo fmt --check`, `cargo clippy --workspace --all-targets --offline -- -D warnings`, `cargo test --workspace --offline`, `sh tests/stress.sh 300`. Because the HTTP server is on the performance-sensitive path, also `python3 bench/run.py --check` and `python3 bench/http/counts.py --check` (never concurrently with a build or tests). The benchmark servers must be unchanged.

### Milestone 1: annotations and docstrings (about 1 day)

- `ast::AnnotationValue` gains `List(Vec<AnnotationValue>)` and `Bool(bool)`; the parser, `fmt.rs` (it keeps tokens), the LSP and the editor grammars need no more than accepting them.
- `check/http.rs` `route_of` accepts `tags=[str, ...]`, `deprecated=True|False`, `errors=[int, ...]` (each 400 to 599) and keeps them on `Route`; a wrong value is a `BAD_ROUTE` error with a help line, like the existing `status=` check. Update the annotation table in docs/05.
- `Route` gets `summary` and `description`, taken from the handler's docstring (first statement of `function.body`, if it is a string expression; dedented as Python's `inspect.cleandoc` does).
- Tests: `tests/ui/route_annotation_errors.wb` (golden), a unit test of the docstring split. Existing programs must compile and run unchanged.

### Milestone 2: schemas and operations (about 2 days)

- New `crates/lang_sema/src/check/openapi.rs`: from a `Route` and the checker's type tables, produce the operation JSON and the component schemas (section 4.2 and 4.3), as text. Reuse the JSON-capability walk in `check/json.rs` so the schema and the encoder cannot disagree; add a test that, for every `@derive(Json)` type in `tests/`, encodes a sample value and checks it against the generated schema in a small validator in the test (jsonschema is not installed and cannot be downloaded: write the subset validator, about 150 lines, covering `type`, `properties`, `required`, `items`, `prefixItems`, `additionalProperties`, `enum`, `oneOf`, `$ref`).
- Name collisions between different types become a new error code (the next free one, `E0341` at the time of writing; add it to `lang_diag/src/codes.rs` and a section to `docs/errors.md`, which a test checks).
- `mount` emits the registration calls only when the program uses `docs` (the token scan). `Server` gets the two private fields and the private methods in `http.lang`.
- Tests: golden JSON for `tests/http/todo_annotations.wb` (`tests/http/todo_annotations.openapi.json`), a program with every schema form in the table, generics, a recursive type, a mounted prefix, two types with a method of the same name. Run the compiler with a debug flag or a test hook that prints the generated documentation source (the same way the mount adapters can be inspected today).
- Gate: the benchmark servers' generated code is byte-identical to before (compare `--emit=llvm-ir` or the object size).

### Milestone 3: `docs`, `openapi_json`, the document (about 1.5 days)

- `Server.docs(...)` and `Server.openapi_json(...)` in `http.lang`: build the document from the stored fragments (section 4.1), cached; JSON as above; YAML from a small emitter in the library working on the same text (quote strings; the document only has objects, arrays, strings, numbers, booleans and null). Call-style routes are included with their path parameters.
- The `docs` routes: `openapi.json`, `openapi.yaml`, and the HTML page, which for now loads Swagger UI from the CDN (`cdn=True` behaviour) so the milestone can ship and be checked without the vendored files.
- HTML is escaped; `title`, `description` and `servers` are JSON-escaped in the document.
- Tests: `crates/lang_cli/tests/http.rs` gains a case that starts the todo service with `docs`, fetches `/docs/openapi.json`, parses it, and checks the paths, parameters, schemas and statuses against the service's real behaviour (e.g. the declared 404 really occurs, the declared 400 really occurs); `/docs/openapi.yaml` parses to the same content (compare through a small YAML subset reader in the test, or by round trip with Python if available in the test environment: the tests may not require Python). A `tests/run/` program using `openapi_json` with an in-process `http.test_request`.

### Milestone 4: the built-in Swagger UI (about 1.5 days; needs the approval in section 5)

- After the owner's approval: fetch the pinned `swagger-ui-dist` tarball, verify its published integrity hash, extract `swagger-ui.css`, `swagger-ui-bundle.js`, `LICENSE`, `NOTICE`, gzip the two files at the highest level, and commit them under `crates/lang_rt/assets/` with the README of section 5. Source maps are not included.
- `lang_rt`: statics with `include_bytes!`, a function returning each asset as a `str`-shaped pointer and length, its ETag, and its content type. The `docs` code serves them (section 4.5), plus the generated `swagger-initializer.js`.
- Tests: an HTTP test fetches each asset with and without `Accept-Encoding: gzip` and `If-None-Match`; `crates/lang_cli/tests/executables.rs` gains two size checks: a program that does not call `docs` is not larger than before (compare the hello and benchmark-server sizes with the recorded ones in `bench/README.md`), and one that does is smaller than a stated bound. **Verify in a browser, not only by tests:** with the built-in browser, start the todo service with `docs`, open `/docs`, expand an operation, press "Try it out" on `GET /todos/{id}`, and take a screenshot; check that the browser's console reports no errors and no request leaves localhost (the network log).
- Measure and write down: the executable's growth, the startup effect (none expected), the page's load time.

### Milestone 5: polish and close-out (about 1 day)

- `security`: `http.SecurityScheme` (bearer, API key in a header or cookie, basic) listed in `components.securitySchemes`, and `security=["name"]` on an annotation to mark an operation (Swagger UI's "Authorize" button works from this). `persist_authorization` and other Swagger UI options are passed through a `ui_options: dict[str, str]` of raw JSON values.
- `tags=[http.Tag(...)]` descriptions and ordering.
- Documentation: a new "OpenAPI and Swagger UI" subsection in docs/05 and in `examples.md` (the program in section 1, compiled by the guide test), the API table, the error code, `docs/release-notes.md` ("What changed" and limitations), `docs/06-roadmap.md`, README feature list, `editors/` snippets if they list annotation names, and the D entries.
- Run the whole gate list, including the benchmark checks, and the browser check again. Update this file with what was built and what was left out.
- Leave to the owner: whether the next version number is 1.1.0 (this is a new feature; do not change the version without asking).

## 7. Risks and open questions

- **Size.** Swagger UI is the largest single item the project would carry. Mitigated by gzip, by dropping unreferenced statics at link time, and by `cdn=True`. If the owner declines to vendor it, the feature is still useful: the JSON and YAML endpoints and the CDN page.
- **The spec follows the encoder.** If the JSON encoding of a type changes later, the schema must change with it; the milestone 2 test that validates encoded samples against the generated schema is what keeps them together. Keep it in the standard test run.
- **Errors from `Result` handlers are not typed.** `Result[T]` carries any `Error`, so which statuses a handler can return is unknown to the compiler; `errors=[...]` is the user's statement, and nothing checks it. (A later improvement could scan for `http.Error(404, ...)` literals, which is fragile; not planned.)
- **Call-style routes** are documented thinly. The cost of richer metadata is an extra argument on every registration method; deferred until there is demand. Annotated routes are the recommended style and are fully described.
- **OpenAPI 3.0.** Some code generators and gateways read only 3.0. A converter (nullable types, `prefixItems`) is a contained follow-up; not part of this plan.
- **`Accept-Encoding: gzip` is required for the built-in assets.** A proxy that strips it would break the page; unlikely, documented in the troubleshooting section.
- **Docs behind authentication.** The docs routes pass through the server's middleware, so an authentication middleware blocks the page along with the API; the browser cannot send a bearer token for the page itself. Document the pattern (let the middleware skip the docs path) in the example.
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

Sources for the research: the `@fastify/swagger` and `@fastify/swagger-ui` READMEs and migration guide on npm and unpkg; FastAPI's "Custom Docs UI Static Assets" how-to; the `swagger-ui-dist` package page on npm (Apache-2.0, "the entire dist folder" as a dependency-free module).
