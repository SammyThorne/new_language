// The API documentation page's logic, with no DOM: what viewer.js shows is worked out here, and
// core.test.js tests it under node (`node --test`). Plain ES2020, no dependencies.
(function (root) {
  "use strict";

  const METHODS = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];
  const MAX_DEPTH = 8;

  /** Follow a local `$ref` such as `#/components/schemas/Todo`; `undefined` if it leads nowhere. */
  function resolveRef(doc, ref) {
    if (typeof ref !== "string" || !ref.startsWith("#/")) return undefined;
    let node = doc;
    for (const raw of ref.slice(2).split("/")) {
      const key = raw.replace(/~1/g, "/").replace(/~0/g, "~");
      if (node === null || typeof node !== "object" || !Object.prototype.hasOwnProperty.call(node, key)) {
        return undefined;
      }
      node = node[key];
    }
    return node;
  }

  /** A schema with its `$ref` chain followed, stopping at a cycle. */
  function resolve(doc, schema) {
    const seen = new Set();
    let current = schema;
    while (current && typeof current === "object" && typeof current.$ref === "string") {
      if (seen.has(current.$ref)) return {};
      seen.add(current.$ref);
      const next = resolveRef(doc, current.$ref);
      if (next === undefined) return {};
      current = next;
    }
    return current === undefined || current === null ? {} : current;
  }

  /** The last part of a `$ref`: `Todo` for `#/components/schemas/Todo`. */
  function refName(ref) {
    return ref.split("/").pop();
  }

  function types(schema) {
    if (!schema || typeof schema !== "object") return [];
    if (Array.isArray(schema.type)) return schema.type;
    return typeof schema.type === "string" ? [schema.type] : [];
  }

  /** An example value for a schema: its default, else something of the right shape. */
  function example(doc, schema, depth) {
    depth = depth || 0;
    if (schema === true || schema === undefined) return null;
    if (schema === false || depth > MAX_DEPTH) return null;
    if (Object.prototype.hasOwnProperty.call(schema, "default")) return schema.default;
    if (Object.prototype.hasOwnProperty.call(schema, "example")) return schema.example;
    if (Object.prototype.hasOwnProperty.call(schema, "const")) return schema.const;
    if (typeof schema.$ref === "string") {
      if (depth >= MAX_DEPTH) return null;
      return example(doc, resolve(doc, schema), depth + 1);
    }
    if (Array.isArray(schema.enum) && schema.enum.length > 0) return schema.enum[0];
    const branches = schema.oneOf || schema.anyOf;
    if (Array.isArray(branches) && branches.length > 0) {
      const first = branches.find((b) => !types(b).every((t) => t === "null")) || branches[0];
      return example(doc, first, depth + 1);
    }
    if (Array.isArray(schema.allOf) && schema.allOf.length > 0) {
      return example(doc, schema.allOf[0], depth + 1);
    }
    const kinds = types(schema).filter((t) => t !== "null");
    const kind = kinds.length > 0 ? kinds[0] : schema.properties ? "object" : undefined;
    switch (kind) {
      case "integer":
        return 0;
      case "number":
        return 0.0;
      case "boolean":
        return false;
      case "string":
        return "";
      case "null":
        return null;
      case "array": {
        if (Array.isArray(schema.prefixItems)) {
          return schema.prefixItems.map((item) => example(doc, item, depth + 1));
        }
        if (depth + 1 > MAX_DEPTH || schema.items === false) return [];
        const item = example(doc, schema.items, depth + 1);
        return item === null && isRecursive(doc, schema.items, depth + 1) ? [] : [item];
      }
      case "object": {
        const result = {};
        const properties = schema.properties || {};
        const required = Array.isArray(schema.required) ? schema.required : [];
        const names = required.concat(Object.keys(properties).filter((n) => !required.includes(n)));
        for (const name of names) {
          if (!Object.prototype.hasOwnProperty.call(properties, name)) continue;
          if (!required.includes(name) && depth + 1 >= MAX_DEPTH) continue;
          result[name] = example(doc, properties[name], depth + 1);
        }
        if (Object.keys(properties).length === 0 && schema.additionalProperties
          && typeof schema.additionalProperties === "object") {
          result.key = example(doc, schema.additionalProperties, depth + 1);
        }
        return result;
      }
      default:
        return null;
    }
  }

  function isRecursive(doc, schema, depth) {
    return depth >= MAX_DEPTH || (schema && typeof schema.$ref === "string");
  }

  /** A short type for tables: `integer`, `string?`, `array<Todo>`, `Todo`. */
  function typeLabel(doc, schema) {
    if (schema === true || schema === undefined || schema === null) return "any";
    if (schema === false) return "never";
    if (typeof schema.$ref === "string") return refName(schema.$ref);
    const branches = schema.oneOf || schema.anyOf;
    if (Array.isArray(branches)) {
      const nonNull = branches.filter((b) => !(types(b).length === 1 && types(b)[0] === "null"));
      const nullable = nonNull.length < branches.length;
      if (nonNull.length === 1) return typeLabel(doc, nonNull[0]) + (nullable ? "?" : "");
      return nonNull.map((b) => b.title || typeLabel(doc, b)).join(" | ") + (nullable ? " | null" : "");
    }
    if (Array.isArray(schema.enum)) return "enum";
    const kinds = types(schema);
    const nullable = kinds.includes("null") && kinds.length > 1;
    const kind = kinds.filter((t) => t !== "null")[0] || (schema.properties ? "object" : "any");
    let label = kind;
    if (kind === "array") {
      if (Array.isArray(schema.prefixItems)) {
        label = "[" + schema.prefixItems.map((s) => typeLabel(doc, s)).join(", ") + "]";
      } else {
        label = "array<" + typeLabel(doc, schema.items) + ">";
      }
    } else if (kind === "object" && schema.additionalProperties && typeof schema.additionalProperties === "object") {
      label = "map<string, " + typeLabel(doc, schema.additionalProperties) + ">";
    }
    return label + (nullable ? "?" : "");
  }

  /**
   * The operations, grouped by tag in the order tags first appear (`doc.tags` order first),
   * each with its parameters merged from the path item and the operation.
   */
  function operations(doc) {
    const groups = new Map();
    for (const tag of Array.isArray(doc.tags) ? doc.tags : []) {
      if (tag && typeof tag.name === "string") {
        groups.set(tag.name, { name: tag.name, description: tag.description || "", operations: [] });
      }
    }
    const paths = doc.paths || {};
    for (const path of Object.keys(paths)) {
      const item = paths[path] || {};
      const shared = Array.isArray(item.parameters) ? item.parameters : [];
      for (const method of METHODS) {
        const op = item[method];
        if (!op || typeof op !== "object") continue;
        const own = Array.isArray(op.parameters) ? op.parameters.map((p) => resolve(doc, p)) : [];
        const merged = own.slice();
        for (const p of shared.map((s) => resolve(doc, s))) {
          if (!merged.some((m) => m.name === p.name && m.in === p.in)) merged.push(p);
        }
        const tags = Array.isArray(op.tags) && op.tags.length > 0 ? op.tags : ["default"];
        const entry = {
          id: op.operationId || method + " " + path,
          method,
          path,
          summary: op.summary || "",
          description: op.description || "",
          deprecated: op.deprecated === true,
          parameters: merged,
          requestBody: op.requestBody ? resolve(doc, op.requestBody) : null,
          responses: op.responses || {},
          security: op.security !== undefined ? op.security : doc.security || [],
          websocket: op["x-websocket"] === true,
          tag: tags[0],
        };
        if (!groups.has(entry.tag)) groups.set(entry.tag, { name: entry.tag, description: "", operations: [] });
        groups.get(entry.tag).operations.push(entry);
      }
    }
    return Array.from(groups.values()).filter((g) => g.operations.length > 0);
  }

  /** Whether an operation matches the filter text (method, path, summary, tag, id). */
  function matches(op, filter) {
    const text = filter.trim().toLowerCase();
    if (text === "") return true;
    return [op.method, op.path, op.summary, op.tag, op.id].some((field) => String(field).toLowerCase().includes(text));
  }

  /**
   * The request an operation makes with the values typed in: `values.params` maps `in:name` to
   * text, `values.body` is the body text, `auth` maps security scheme names to credentials.
   */
  function buildRequest(doc, op, values, baseUrl, auth) {
    const params = (values && values.params) || {};
    let path = op.path;
    const query = [];
    const headers = {};
    const cookies = [];
    const missing = [];
    for (const p of op.parameters) {
      const key = p.in + ":" + p.name;
      const value = Object.prototype.hasOwnProperty.call(params, key) ? String(params[key]) : "";
      if (value === "") {
        if (p.required) missing.push(p.name);
        continue;
      }
      if (p.in === "path") {
        path = path.split("{" + p.name + "}").join(encodeURIComponent(value));
      } else if (p.in === "query") {
        query.push(encodeURIComponent(p.name) + "=" + encodeURIComponent(value));
      } else if (p.in === "header") {
        headers[p.name] = value;
      } else if (p.in === "cookie") {
        cookies.push(p.name + "=" + encodeURIComponent(value));
      }
    }
    applyAuth(doc, op, auth || {}, headers, query, cookies);
    if (cookies.length > 0) headers.Cookie = cookies.join("; ");
    let body = null;
    if (op.requestBody) {
      const text = values && typeof values.body === "string" ? values.body : "";
      if (text.trim() !== "") {
        body = text;
        headers["Content-Type"] = "application/json";
      } else if (op.requestBody.required) {
        missing.push("request body");
      }
    }
    const base = String(baseUrl || "").replace(/\/+$/, "");
    const url = base + path + (query.length > 0 ? "?" + query.join("&") : "");
    return { method: op.method.toUpperCase(), url, headers, body, missing };
  }

  /** Add the credentials of the security schemes the operation uses. */
  function applyAuth(doc, op, auth, headers, query, cookies) {
    const schemes = (doc.components && doc.components.securitySchemes) || {};
    const requirements = Array.isArray(op.security) ? op.security : [];
    for (const requirement of requirements) {
      for (const name of Object.keys(requirement || {})) {
        const scheme = schemes[name];
        const value = auth[name];
        if (!scheme || typeof value !== "string" || value === "") continue;
        if (scheme.type === "http" && String(scheme.scheme).toLowerCase() === "bearer") {
          headers.Authorization = "Bearer " + value;
        } else if (scheme.type === "http" && String(scheme.scheme).toLowerCase() === "basic") {
          headers.Authorization = "Basic " + base64(value);
        } else if (scheme.type === "apiKey" && scheme.in === "header") {
          headers[scheme.name] = value;
        } else if (scheme.type === "apiKey" && scheme.in === "query") {
          query.push(encodeURIComponent(scheme.name) + "=" + encodeURIComponent(value));
        } else if (scheme.type === "apiKey" && scheme.in === "cookie") {
          cookies.push(scheme.name + "=" + encodeURIComponent(value));
        }
      }
    }
  }

  /** Base64 of a string's UTF-8 bytes, for basic authentication. */
  function base64(text) {
    const bytes = new TextEncoder().encode(text);
    const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let out = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const n = (bytes[i] << 16) | ((bytes[i + 1] || 0) << 8) | (bytes[i + 2] || 0);
      out += alphabet[(n >> 18) & 63] + alphabet[(n >> 12) & 63];
      out += i + 1 < bytes.length ? alphabet[(n >> 6) & 63] : "=";
      out += i + 2 < bytes.length ? alphabet[n & 63] : "=";
    }
    return out;
  }

  /** A shell word: single-quoted, with single quotes written as `'\''`. */
  function shellQuote(text) {
    return "'" + String(text).split("'").join("'\\''") + "'";
  }

  /** The request as a `curl` command. */
  function curl(request) {
    const parts = ["curl", "-X", request.method, shellQuote(request.url)];
    for (const name of Object.keys(request.headers)) {
      parts.push("-H", shellQuote(name + ": " + request.headers[name]));
    }
    if (request.body !== null && request.body !== undefined) parts.push("--data", shellQuote(request.body));
    return parts.join(" ");
  }

  function escapeHtml(text) {
    return String(text)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  /** Whether a link target is safe: http, https, mailto, or relative (no other scheme). */
  function safeHref(target) {
    const trimmed = target.trim();
    if (/^(https?:|mailto:)/i.test(trimmed)) return true;
    return !/^[a-z][a-z0-9+.-]*:/i.test(trimmed) && !trimmed.startsWith("//");
  }

  /** Inline Markdown on escaped text: `code`, **bold**, *emphasis*, [links](url). */
  function inline(text) {
    const pieces = text.split("`");
    // An unbalanced backtick is kept as text.
    if (pieces.length % 2 === 0) pieces[pieces.length - 2] += "`" + pieces.pop();
    return pieces
      .map((piece, index) => {
        if (index % 2 === 1) return "<code>" + escapeHtml(piece) + "</code>";
        let html = escapeHtml(piece);
        html = html.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
        html = html.replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>");
        html = html.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (whole, label, target) => {
          const raw = target.replace(/&amp;/g, "&");
          if (!safeHref(raw) || /&(lt|gt|quot|#39);/.test(target)) return whole;
          return '<a href="' + target + '" rel="noopener noreferrer">' + label + "</a>";
        });
        return html;
      })
      .join("");
  }

  /**
   * A small, safe Markdown: paragraphs, fenced code blocks, `-` and `*` lists, and the inline
   * forms above. Everything is escaped first; no HTML in the text survives as HTML.
   */
  function markdown(text) {
    const lines = String(text || "").replace(/\r\n?/g, "\n").split("\n");
    const out = [];
    let paragraph = [];
    let list = [];
    const flush = () => {
      if (paragraph.length > 0) out.push("<p>" + inline(paragraph.join(" ")) + "</p>");
      if (list.length > 0) out.push("<ul>" + list.map((item) => "<li>" + inline(item) + "</li>").join("") + "</ul>");
      paragraph = [];
      list = [];
    };
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (/^\s*```/.test(line)) {
        flush();
        const code = [];
        i++;
        while (i < lines.length && !/^\s*```/.test(lines[i])) code.push(lines[i++]);
        out.push("<pre><code>" + escapeHtml(code.join("\n")) + "</code></pre>");
        continue;
      }
      const item = /^\s*[-*]\s+(.*)$/.exec(line);
      if (item) {
        if (paragraph.length > 0) {
          out.push("<p>" + inline(paragraph.join(" ")) + "</p>");
          paragraph = [];
        }
        list.push(item[1]);
      } else if (line.trim() === "") {
        flush();
      } else if (list.length > 0 && /^\s+/.test(line)) {
        list[list.length - 1] += " " + line.trim();
      } else {
        if (list.length > 0) flush();
        paragraph.push(line.trim());
      }
    }
    flush();
    return out.join("");
  }

  /** A JSON text, indented, if `text` is JSON; otherwise the text unchanged. */
  function prettyJson(text) {
    try {
      return JSON.stringify(JSON.parse(text), null, 2);
    } catch (_) {
      return text;
    }
  }

  /** What a security scheme asks for, in words: "bearer token", "API key in header X-Key". */
  function schemeLabel(scheme) {
    if (!scheme || typeof scheme !== "object") return "unknown scheme";
    const kind = String(scheme.scheme || "").toLowerCase();
    if (scheme.type === "http" && kind === "bearer") return "bearer token";
    if (scheme.type === "http" && kind === "basic") return "user:password (basic)";
    if (scheme.type === "apiKey") return "API key in " + scheme.in + " " + scheme.name;
    return String(scheme.type || "unknown scheme");
  }

  /** The names of the schemes an operation accepts, any one of them; empty when it is open. */
  function requirements(op) {
    const names = [];
    for (const requirement of Array.isArray(op.security) ? op.security : []) {
      for (const name of Object.keys(requirement || {})) if (!names.includes(name)) names.push(name);
    }
    return names;
  }

  /** The `id` of an operation as a URL fragment, and back. */
  function anchor(op) {
    return "op-" + encodeURIComponent(op.id).replace(/%/g, "_");
  }

  const api = {
    METHODS,
    resolve,
    resolveRef,
    refName,
    example,
    typeLabel,
    operations,
    matches,
    buildRequest,
    curl,
    shellQuote,
    escapeHtml,
    safeHref,
    markdown,
    prettyJson,
    anchor,
    base64,
    schemeLabel,
    requirements,
  };
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.ApiDocs = api;
})(typeof self !== "undefined" ? self : this);
