// The API documentation page: builds the page from the OpenAPI document with DOM calls. All text
// from the document goes in as text nodes; the only HTML made from it comes from
// ApiDocs.markdown, which escapes everything first. Plain ES2020, no dependencies.
(function () {
  "use strict";

  const core = window.ApiDocs;
  const meta = (name) => {
    const tag = document.querySelector('meta[name="' + name + '"]');
    return tag ? tag.getAttribute("content") : "";
  };
  const documentUrl = meta("openapi-url");
  const tryItOut = meta("try-it-out") === "true";
  /** Credentials typed into "Authorize", by security scheme name: in memory only. */
  const auth = {};
  let doc = null;
  let baseUrl = "";

  /** An element: `h("a", {href: "#x", class: "c"}, "text", child)`. */
  function h(tag, attrs, ...children) {
    const element = document.createElement(tag);
    for (const [name, value] of Object.entries(attrs || {})) {
      if (value === undefined || value === null || value === false) continue;
      if (name.startsWith("on")) element.addEventListener(name.slice(2), value);
      else if (value === true) element.setAttribute(name, "");
      else element.setAttribute(name, String(value));
    }
    for (const child of children.flat()) {
      if (child === null || child === undefined || child === false) continue;
      element.append(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return element;
  }

  /** Markdown from the document, through the one function that makes HTML from it. */
  function prose(text, className) {
    const element = h("div", { class: "prose " + (className || "") });
    element.innerHTML = core.markdown(text);
    return element;
  }

  // ---- schemas ------------------------------------------------------------------------------

  /** A schema as a tree: object properties with their types, nested types on demand. */
  function schemaView(schema, depth) {
    depth = depth || 0;
    if (schema && typeof schema.$ref === "string") {
      const name = core.refName(schema.$ref);
      return lazyDetails(h("span", { class: "type" }, name), () => schemaView(core.resolve(doc, schema), depth + 1), "model-ref");
    }
    const box = h("div", { class: "schema" });
    if (!schema || typeof schema !== "object") {
      box.append(h("span", { class: "type" }, core.typeLabel(doc, schema)));
      return box;
    }
    if (schema.description) box.append(prose(schema.description));
    const branches = schema.oneOf || schema.anyOf;
    if (Array.isArray(branches)) {
      box.append(h("div", { class: "note" }, "One of:"));
      const list = h("ul", { class: "branches" });
      for (const branch of branches) {
        const label = branch.title || core.typeLabel(doc, branch);
        const simple = !branch.properties && !branch.$ref && !branch.items;
        list.append(h("li", {}, simple ? h("span", { class: "type" }, label) : lazyDetails(h("span", { class: "type" }, label), () => schemaView(branch, depth + 1))));
      }
      box.append(list);
      if (schema.discriminator && schema.discriminator.propertyName) {
        box.append(h("div", { class: "note" }, "Told apart by ", h("code", {}, schema.discriminator.propertyName), "."));
      }
      return box;
    }
    if (Array.isArray(schema.enum)) {
      box.append(h("div", {}, h("span", { class: "type" }, core.typeLabel(doc, { ...schema, enum: undefined })), " one of ",
        schema.enum.map((value, i) => [i > 0 ? ", " : "", h("code", {}, JSON.stringify(value))])));
      return box;
    }
    const properties = schema.properties || {};
    const names = Object.keys(properties);
    if (names.length === 0) {
      const label = core.typeLabel(doc, schema);
      const inner = schema.items || (typeof schema.additionalProperties === "object" ? schema.additionalProperties : null);
      if (inner && (inner.$ref || inner.properties || inner.oneOf)) {
        box.append(h("span", { class: "type" }, label), lazyDetails(h("span", { class: "muted" }, "item"), () => schemaView(inner, depth + 1)));
      } else {
        box.append(h("span", { class: "type" }, label));
      }
      return box;
    }
    const required = Array.isArray(schema.required) ? schema.required : [];
    const table = h("table", { class: "fields" }, h("thead", {}, h("tr", {}, h("th", {}, "Field"), h("th", {}, "Type"), h("th", {}, "Notes"))));
    const body = h("tbody");
    for (const name of names) {
      const property = properties[name];
      const notes = [];
      if (required.includes(name)) notes.push(h("span", { class: "required" }, "required"));
      if (property && Object.prototype.hasOwnProperty.call(property, "default")) {
        notes.push(h("span", {}, "default ", h("code", {}, JSON.stringify(property.default))));
      }
      if (property && property.description) notes.push(prose(property.description));
      const nested = property && (property.$ref || property.properties || property.oneOf
        || (property.items && (property.items.$ref || property.items.properties)));
      body.append(h("tr", {},
        h("td", {}, h("code", {}, name)),
        h("td", {}, nested
          ? lazyDetails(h("span", { class: "type" }, core.typeLabel(doc, property)), () => schemaView(inside(property), depth + 1))
          : h("span", { class: "type" }, core.typeLabel(doc, property))),
        h("td", {}, notes)));
    }
    table.append(body);
    if (schema.additionalProperties === false) box.append(h("div", { class: "note" }, "No other fields are accepted."));
    box.prepend(table);
    return box;
  }

  /** What to show when a field's type is opened: the referenced schema, or an array's items. */
  function inside(schema) {
    if (schema.$ref) return core.resolve(doc, schema);
    if (schema.items && !schema.properties && !schema.oneOf) return inside(schema.items);
    return schema;
  }

  /** A `<details>` whose content is built the first time it opens (schemas can be recursive). */
  function lazyDetails(summary, build, className) {
    const details = h("details", { class: "lazy " + (className || "") }, h("summary", {}, summary));
    details.addEventListener("toggle", () => {
      if (details.open && details.childElementCount === 1) details.append(build());
    });
    return details;
  }

  // ---- operations ---------------------------------------------------------------------------

  function operationView(op) {
    const details = h("details", { class: "op op-" + op.method + (op.deprecated ? " deprecated" : ""), id: core.anchor(op) });
    details.append(h("summary", {},
      h("span", { class: "method" }, op.websocket ? "WS" : op.method.toUpperCase()),
      h("code", { class: "path" }, op.path),
      h("span", { class: "summary" }, op.summary),
      op.deprecated ? h("span", { class: "badge" }, "deprecated") : null));
    let built = false;
    details.addEventListener("toggle", () => {
      if (details.open) {
        if (!built) {
          details.append(operationBody(op));
          built = true;
        }
        history.replaceState(null, "", "#" + core.anchor(op));
      }
    });
    return details;
  }

  function operationBody(op) {
    const body = h("div", { class: "op-body" });
    if (op.description) body.append(prose(op.description));
    if (op.websocket) body.append(h("p", { class: "note" }, "A WebSocket endpoint: connect with a WebSocket client; it cannot be tried from this page."));
    if (op.parameters.length > 0) {
      const rows = op.parameters.map((p) => h("tr", {},
        h("td", {}, h("code", {}, p.name)),
        h("td", {}, p.in),
        h("td", {}, h("span", { class: "type" }, core.typeLabel(doc, p.schema))),
        h("td", {}, p.required ? h("span", { class: "required" }, "required") : "optional",
          p.schema && Object.prototype.hasOwnProperty.call(p.schema, "default") ? [" · default ", h("code", {}, JSON.stringify(p.schema.default))] : null,
          p.description ? prose(p.description) : null)));
      body.append(h("h4", {}, "Parameters"),
        h("table", { class: "params" }, h("thead", {}, h("tr", {}, h("th", {}, "Name"), h("th", {}, "In"), h("th", {}, "Type"), h("th", {}, ""))), h("tbody", {}, rows)));
    }
    if (op.requestBody) {
      body.append(h("h4", {}, "Request body", op.requestBody.required ? h("span", { class: "required" }, " required") : null));
      for (const [media, content] of Object.entries(op.requestBody.content || {})) {
        body.append(h("div", { class: "media" }, h("code", {}, media)), schemaView(content.schema));
      }
    }
    const responses = h("table", { class: "responses" }, h("thead", {}, h("tr", {}, h("th", {}, "Status"), h("th", {}, "Description"), h("th", {}, "Body"))));
    const rows = h("tbody");
    for (const [status, raw] of Object.entries(op.responses)) {
      const response = core.resolve(doc, raw);
      const content = response.content || {};
      const media = Object.keys(content);
      rows.append(h("tr", {},
        h("td", {}, h("span", { class: "status status-" + String(status)[0] }, status)),
        h("td", {}, response.description || ""),
        h("td", {}, media.map((m) => h("div", {}, h("code", {}, m), content[m].schema ? schemaView(content[m].schema) : null)))));
    }
    responses.append(rows);
    body.append(h("h4", {}, "Responses"), responses);
    if (tryItOut && !op.websocket) body.append(tryView(op));
    return body;
  }

  // ---- try it out ---------------------------------------------------------------------------

  function tryView(op) {
    const panel = h("div", { class: "try", hidden: true });
    const open = h("button", { type: "button", class: "secondary", onclick: () => { panel.hidden = !panel.hidden; open.textContent = panel.hidden ? "Try it out" : "Close"; } }, "Try it out");
    const inputs = {};
    const fields = h("div", { class: "form" });
    for (const p of op.parameters) {
      const id = core.anchor(op) + "-" + p.in + "-" + p.name;
      const schema = p.schema || {};
      let input;
      if (schema.type === "boolean" || (Array.isArray(schema.enum) && schema.enum.length < 20)) {
        const choices = schema.type === "boolean" ? ["true", "false"] : schema.enum.map(String);
        input = h("select", { id }, h("option", { value: "" }, p.required ? "choose" : "(none)"), choices.map((c) => h("option", { value: c }, c)));
      } else {
        input = h("input", { id, type: "text", autocomplete: "off", placeholder: core.typeLabel(doc, schema) });
      }
      if (Object.prototype.hasOwnProperty.call(schema, "default")) input.value = String(schema.default);
      inputs[p.in + ":" + p.name] = input;
      fields.append(h("label", { for: id }, h("code", {}, p.name), " ", h("span", { class: "muted" }, p.in + (p.required ? ", required" : ""))), input);
    }
    let bodyInput = null;
    if (op.requestBody) {
      const id = core.anchor(op) + "-body";
      const json = (op.requestBody.content || {})["application/json"];
      const sample = json ? core.example(doc, json.schema) : "";
      bodyInput = h("textarea", { id, rows: 8, spellcheck: "false" });
      bodyInput.value = json ? JSON.stringify(sample, null, 2) : "";
      fields.append(h("label", { for: id }, "Request body ", h("span", { class: "muted" }, "JSON")), bodyInput);
    }
    const result = h("div", { class: "result", "aria-live": "polite" });
    const send = h("button", { type: "button", class: "primary", onclick: () => run(op, inputs, bodyInput, result, send) }, "Send");
    const reset = h("button", { type: "button", class: "secondary", onclick: () => { result.replaceChildren(); } }, "Clear");
    panel.append(fields, h("div", { class: "actions" }, send, reset), result);
    return h("div", {}, open, panel);
  }

  async function run(op, inputs, bodyInput, result, button) {
    const values = { params: {}, body: bodyInput ? bodyInput.value : "" };
    for (const [key, input] of Object.entries(inputs)) values.params[key] = input.value;
    const request = core.buildRequest(doc, op, values, baseUrl, auth);
    result.replaceChildren();
    if (request.missing.length > 0) {
      result.append(h("p", { class: "error" }, "Fill in: " + request.missing.join(", ")));
      return;
    }
    if (request.body !== null) {
      try {
        JSON.parse(request.body);
      } catch (error) {
        result.append(h("p", { class: "error" }, "The body is not valid JSON: " + error.message));
        return;
      }
    }
    const command = core.curl({ ...request, url: new URL(request.url, location.href).href });
    button.disabled = true;
    const started = performance.now();
    try {
      const response = await fetch(request.url, { method: request.method, headers: request.headers, body: request.body === null ? undefined : request.body, credentials: "same-origin" });
      const text = await response.text();
      const elapsed = Math.round(performance.now() - started);
      const headers = [];
      response.headers.forEach((value, name) => headers.push(name + ": " + value));
      result.append(
        h("div", { class: "outcome" }, h("span", { class: "status status-" + String(response.status)[0] }, String(response.status)), " ", response.statusText, h("span", { class: "muted" }, " · " + elapsed + " ms")),
        h("h5", {}, "Body"), h("pre", { class: "code" }, core.prettyJson(text) || "(empty)"),
        h("h5", {}, "Headers"), h("pre", { class: "code" }, headers.join("\n")));
    } catch (error) {
      result.append(h("p", { class: "error" }, "The request failed: " + error.message + ". (A server on another origin must allow it with CORS.)"));
    } finally {
      button.disabled = false;
    }
    const copy = h("button", { type: "button", class: "secondary small", onclick: () => copyText(command, copy) }, "Copy");
    result.append(h("h5", {}, "curl ", copy), h("pre", { class: "code" }, command));
  }

  function copyText(text, button) {
    const done = () => { button.textContent = "Copied"; setTimeout(() => { button.textContent = "Copy"; }, 1500); };
    if (navigator.clipboard) navigator.clipboard.writeText(text).then(done, () => {});
  }

  // ---- authorize ----------------------------------------------------------------------------

  function authorizeView(schemes) {
    const panel = h("section", { class: "authorize", hidden: true, "aria-label": "Authorize" });
    panel.append(h("p", { class: "note" }, "Credentials are kept in this page only, and added to the requests it sends."));
    for (const [name, scheme] of Object.entries(schemes)) {
      const id = "auth-" + name;
      let what = scheme.type;
      if (scheme.type === "http") what = scheme.scheme === "basic" ? "user:password (basic)" : "bearer token";
      if (scheme.type === "apiKey") what = "API key in " + scheme.in + " " + scheme.name;
      const input = h("input", { id, type: "password", autocomplete: "off", placeholder: what });
      input.addEventListener("input", () => { auth[name] = input.value; });
      panel.append(h("label", { for: id }, h("strong", {}, name), " ", h("span", { class: "muted" }, what)), input);
      if (scheme.description) panel.append(prose(scheme.description));
    }
    return panel;
  }

  // ---- the page -----------------------------------------------------------------------------

  function render() {
    const app = document.getElementById("app");
    app.replaceChildren();
    const info = doc.info || {};
    document.title = (info.title || "API") + " · API documentation";
    const heading = document.getElementById("title");
    heading.textContent = info.title || "API";
    const header = document.getElementById("info");
    header.replaceChildren(
      h("span", { class: "badge" }, "version " + (info.version || "?")),
      h("span", { class: "badge" }, "OpenAPI " + (doc.openapi || "?")),
      h("a", { href: documentUrl }, "openapi.json"),
      h("a", { href: documentUrl.replace(/\.json$/, ".yaml") }, "openapi.yaml"));

    const servers = Array.isArray(doc.servers) ? doc.servers.map((s) => s.url).filter(Boolean) : [];
    baseUrl = servers.length > 0 ? servers[0] : "";
    const toolbar = h("div", { class: "toolbar" });
    const filter = h("input", { type: "search", id: "filter", placeholder: "Filter by path, method, summary or tag", "aria-label": "Filter operations" });
    toolbar.append(filter);
    if (servers.length > 1 && tryItOut) {
      const select = h("select", { "aria-label": "Server", onchange: () => { baseUrl = select.value; } }, servers.map((url) => h("option", { value: url }, url)));
      toolbar.append(select);
    }
    const schemes = (doc.components && doc.components.securitySchemes) || {};
    let authorize = null;
    if (Object.keys(schemes).length > 0 && tryItOut) {
      authorize = authorizeView(schemes);
      toolbar.append(h("button", { type: "button", class: "secondary", "aria-expanded": "false", onclick: (event) => {
        authorize.hidden = !authorize.hidden;
        event.target.setAttribute("aria-expanded", String(!authorize.hidden));
      } }, "Authorize"));
    }
    app.append(toolbar);
    if (authorize) app.append(authorize);
    if (info.description) app.append(prose(info.description, "intro"));

    const groups = core.operations(doc);
    const views = [];
    for (const group of groups) {
      const section = h("section", { class: "group" });
      const list = h("div", { class: "ops" });
      const toggle = h("button", { type: "button", class: "group-toggle", "aria-expanded": "true", onclick: () => {
        list.hidden = !list.hidden;
        toggle.setAttribute("aria-expanded", String(!list.hidden));
      } }, group.name);
      section.append(h("h2", {}, toggle));
      if (group.description) section.append(prose(group.description));
      for (const op of group.operations) {
        const view = operationView(op);
        views.push({ op, view, section });
        list.append(view);
      }
      section.append(list);
      app.append(section);
    }
    if (groups.length === 0) app.append(h("p", { class: "note" }, "This server has no routes to show."));
    const empty = h("p", { class: "note", hidden: true }, "No operation matches the filter.");
    app.append(empty);
    filter.addEventListener("input", () => {
      let shown = 0;
      for (const { op, view } of views) {
        view.hidden = !core.matches(op, filter.value);
        if (!view.hidden) shown++;
      }
      for (const section of new Set(views.map((v) => v.section))) {
        section.hidden = views.every((v) => v.section !== section || v.view.hidden);
      }
      empty.hidden = shown > 0;
    });

    const schemas = (doc.components && doc.components.schemas) || {};
    if (Object.keys(schemas).length > 0) {
      const models = h("section", { class: "group models" }, h("h2", {}, "Models"));
      for (const name of Object.keys(schemas)) {
        const view = lazyDetails(h("span", { class: "type" }, name), () => schemaView(schemas[name]), "model");
        view.id = "model-" + name;
        models.append(view);
      }
      app.append(models);
    }
    openFromHash();
  }

  function openFromHash() {
    const id = decodeURIComponent(location.hash.slice(1));
    if (!id) return;
    const target = document.getElementById(id);
    if (target && target.tagName === "DETAILS") {
      target.open = true;
      target.scrollIntoView();
    }
  }

  async function load() {
    const app = document.getElementById("app");
    try {
      const response = await fetch(documentUrl, { headers: { Accept: "application/json" }, credentials: "same-origin" });
      if (!response.ok) throw new Error("the server answered " + response.status);
      doc = await response.json();
      render();
    } catch (error) {
      app.replaceChildren(h("p", { class: "error" }, "Could not load the API description from " + documentUrl + ": " + error.message));
    }
  }

  window.addEventListener("hashchange", openFromHash);
  load();
})();
