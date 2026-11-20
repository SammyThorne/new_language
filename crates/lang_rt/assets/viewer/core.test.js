// Tests of core.js, run with `node --test` (no packages). crates/lang_cli/tests/viewer.rs runs
// them when node is installed.
"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const core = require("./core.js");

const doc = {
  openapi: "3.1.0",
  info: { title: "T", version: "1" },
  tags: [{ name: "todos", description: "The **todos**." }],
  paths: {
    "/todos/{id}": {
      parameters: [{ name: "id", in: "path", required: true, schema: { type: "integer" } }],
      get: {
        operationId: "get",
        tags: ["todos"],
        summary: "Fetch one",
        parameters: [
          { name: "verbose", in: "query", required: false, schema: { type: "boolean" } },
          { name: "X-Trace", in: "header", required: false, schema: { type: "string" } },
        ],
        responses: { 200: { description: "OK" } },
      },
      patch: {
        operationId: "update",
        tags: ["todos"],
        requestBody: { required: true, content: { "application/json": { schema: { $ref: "#/components/schemas/Todo" } } } },
        responses: { 200: { description: "OK" } },
        security: [{ token: [] }],
      },
    },
    "/health": { get: { operationId: "health", responses: { 200: { description: "OK" } } } },
  },
  components: {
    schemas: {
      Todo: {
        type: "object",
        required: ["id", "title"],
        properties: {
          id: { type: "integer", format: "int64" },
          title: { type: "string" },
          done: { type: "boolean", default: false },
          note: { type: ["string", "null"] },
        },
      },
      Node: {
        type: "object",
        required: ["value"],
        properties: {
          value: { type: "integer" },
          children: { type: "array", items: { $ref: "#/components/schemas/Node" } },
          parent: { oneOf: [{ $ref: "#/components/schemas/Node" }, { type: "null" }] },
        },
      },
      Loop: { $ref: "#/components/schemas/Loop2" },
      Loop2: { $ref: "#/components/schemas/Loop" },
      Shape: {
        oneOf: [
          { type: "object", title: "Circle", required: ["type", "r"], properties: { type: { const: "Circle" }, r: { type: "number" } } },
          { type: "object", title: "Empty", required: ["type"], properties: { type: { const: "Empty" } } },
        ],
      },
      Color: { type: "string", enum: ["Red", "Green"] },
    },
    securitySchemes: {
      token: { type: "http", scheme: "bearer" },
      key: { type: "apiKey", in: "header", name: "X-Key" },
      login: { type: "http", scheme: "basic" },
    },
  },
};

test("resolve follows references and stops at cycles", () => {
  assert.equal(core.resolve(doc, { $ref: "#/components/schemas/Todo" }), doc.components.schemas.Todo);
  assert.deepEqual(core.resolve(doc, { $ref: "#/components/schemas/Loop" }), {});
  assert.deepEqual(core.resolve(doc, { $ref: "#/components/schemas/Missing" }), {});
  assert.deepEqual(core.resolve(doc, { $ref: "https://elsewhere/x" }), {});
  assert.equal(core.resolveRef({ a: { "b/c": 1 } }, "#/a/b~1c"), 1);
});

test("example builds values from schemas", () => {
  assert.deepEqual(core.example(doc, { $ref: "#/components/schemas/Todo" }), { id: 0, title: "", done: false, note: "" });
  assert.equal(core.example(doc, { $ref: "#/components/schemas/Color" }), "Red");
  assert.deepEqual(core.example(doc, { $ref: "#/components/schemas/Shape" }), { type: "Circle", r: 0 });
  assert.deepEqual(core.example(doc, { type: "array", prefixItems: [{ type: "integer" }, { type: "string" }] }), [0, ""]);
  assert.deepEqual(core.example(doc, { type: "object", additionalProperties: { type: "number" } }), { key: 0 });
  assert.deepEqual(core.example(doc, {}), null);
  assert.equal(core.example(doc, { $ref: "#/components/schemas/Loop" }), null);
});

test("example stops on a recursive schema", () => {
  const node = core.example(doc, { $ref: "#/components/schemas/Node" });
  assert.equal(node.value, 0);
  assert.ok(Array.isArray(node.children));
  // Bounded: serialising does not run away.
  assert.ok(JSON.stringify(node).length < 10000);
});

test("typeLabel names types for the tables", () => {
  assert.equal(core.typeLabel(doc, { type: "integer" }), "integer");
  assert.equal(core.typeLabel(doc, { type: ["string", "null"] }), "string?");
  assert.equal(core.typeLabel(doc, { type: "array", items: { $ref: "#/components/schemas/Todo" } }), "array<Todo>");
  assert.equal(core.typeLabel(doc, { oneOf: [{ $ref: "#/components/schemas/Node" }, { type: "null" }] }), "Node?");
  assert.equal(core.typeLabel(doc, { type: "object", additionalProperties: { type: "string" } }), "map<string, string>");
  assert.equal(core.typeLabel(doc, { type: "array", prefixItems: [{ type: "integer" }, { type: "string" }] }), "[integer, string]");
  assert.equal(core.typeLabel(doc, {}), "any");
  assert.equal(core.typeLabel(doc, doc.components.schemas.Shape), "Circle | Empty");
});

test("operations are grouped by tag with merged parameters", () => {
  const groups = core.operations(doc);
  assert.deepEqual(groups.map((g) => g.name), ["todos", "default"]);
  const get = groups[0].operations[0];
  assert.equal(get.id, "get");
  assert.deepEqual(get.parameters.map((p) => p.name), ["verbose", "X-Trace", "id"]);
  assert.equal(groups[0].operations[1].requestBody.required, true);
  assert.equal(groups[1].operations[0].path, "/health");
  assert.ok(core.matches(get, "FETCH"));
  assert.ok(core.matches(get, "todos/{"));
  assert.ok(!core.matches(get, "health"));
});

test("buildRequest encodes parameters and leaves out empty optional ones", () => {
  const get = core.operations(doc)[0].operations[0];
  const request = core.buildRequest(doc, get, { params: { "path:id": "a b/c?", "query:verbose": "" } }, "http://h:1/");
  assert.equal(request.url, "http://h:1/todos/a%20b%2Fc%3F");
  assert.deepEqual(request.headers, {});
  assert.deepEqual(request.missing, []);
  const traced = core.buildRequest(doc, get, { params: { "path:id": "1", "query:verbose": "true & more", "header:X-Trace": "t" } }, "");
  assert.equal(traced.url, "/todos/1?verbose=true%20%26%20more");
  assert.deepEqual(traced.headers, { "X-Trace": "t" });
  const missing = core.buildRequest(doc, get, { params: {} }, "");
  assert.deepEqual(missing.missing, ["id"]);
});

test("buildRequest sends a JSON body and credentials", () => {
  const update = core.operations(doc)[0].operations[1];
  const request = core.buildRequest(doc, update, { params: { "path:id": "2" }, body: '{"title":"x"}' }, "", { token: "abc" });
  assert.equal(request.method, "PATCH");
  assert.equal(request.body, '{"title":"x"}');
  assert.deepEqual(request.headers, { Authorization: "Bearer abc", "Content-Type": "application/json" });
  const empty = core.buildRequest(doc, update, { params: { "path:id": "2" }, body: "  " }, "");
  assert.deepEqual(empty.missing, ["request body"]);
  assert.equal(empty.body, null);
  const keyed = core.buildRequest(doc, { ...update, security: [{ key: [] }, { login: [] }] }, { params: { "path:id": "2" } }, "", { key: "k", login: "ada:pw" });
  assert.equal(keyed.headers["X-Key"], "k");
  assert.equal(keyed.headers.Authorization, "Basic " + Buffer.from("ada:pw").toString("base64"));
});

test("curl quotes single quotes", () => {
  const command = core.curl({ method: "POST", url: "http://h/x?a=1&b=2", headers: { "Content-Type": "application/json" }, body: `{"name":"O'Brien"}` });
  assert.equal(command, `curl -X POST 'http://h/x?a=1&b=2' -H 'Content-Type: application/json' --data '{"name":"O'\\''Brien"}'`);
  assert.equal(core.curl({ method: "GET", url: "/", headers: {}, body: null }), "curl -X GET '/'");
});

test("markdown is safe with hostile input", () => {
  assert.equal(core.markdown("<script>alert(1)</script>"), "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>");
  assert.equal(core.markdown("[x](javascript:alert(1))"), "<p>[x](javascript:alert(1))</p>");
  assert.equal(core.markdown("[x](JaVaScRiPt:alert(1))"), "<p>[x](JaVaScRiPt:alert(1))</p>");
  assert.equal(core.markdown("[x](data:text/html,hi)"), "<p>[x](data:text/html,hi)</p>");
  assert.equal(core.markdown('[x](/a"onmouseover="b)'), "<p>[x](/a&quot;onmouseover=&quot;b)</p>");
  assert.equal(core.markdown("`<b>` and `open"), "<p><code>&lt;b&gt;</code> and `open</p>");
  assert.ok(!core.markdown("<img src=x onerror=alert(1)>").includes("<img"));
  assert.equal(core.markdown("```\n<b>\n```"), "<pre><code>&lt;b&gt;</code></pre>");
});

test("markdown formats what descriptions use", () => {
  assert.equal(
    core.markdown("Fetch **one**\ntodo.\n\n- a `b`\n- [docs](https://x.test/a?b=1&c=2)"),
    '<p>Fetch <strong>one</strong> todo.</p><ul><li>a <code>b</code></li><li><a href="https://x.test/a?b=1&amp;c=2" rel="noopener noreferrer">docs</a></li></ul>',
  );
  assert.equal(core.markdown("*em* and 2 * 3 * 4"), "<p><em>em</em> and 2 * 3 * 4</p>");
  assert.equal(core.markdown("[rel](/todos)"), '<p><a href="/todos" rel="noopener noreferrer">rel</a></p>');
  assert.equal(core.markdown(""), "");
});

test("helpers", () => {
  assert.equal(core.prettyJson('{"a":1}'), '{\n  "a": 1\n}');
  assert.equal(core.prettyJson("not json"), "not json");
  assert.equal(core.anchor({ id: "get one" }), "op-get_20one");
  assert.equal(core.escapeHtml(`<a href="x">'&`), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;");
  assert.equal(core.base64("ab"), "YWI=");
});
