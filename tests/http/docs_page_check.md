# Checking the documentation page by hand

The automatic tests cover the document, the files and their headers (`crates/lang_cli/tests/openapi.rs`), the page's logic (`core.test.js`, run by `crates/lang_cli/tests/viewer.rs` when `node` is installed) and, when the `VIEWER_BROWSER` variable (with the language's prefix) names a Chromium, that the page builds every operation in a headless browser. What they cannot see is how the page looks and behaves. Run this before a release, or after changing anything in `crates/lang_rt/assets/viewer/`.

## Start the todo service

```sh
cargo build --offline -p lang_rt -p lang_cli
target/debug/weby build tests/http/todo_annotations.wb -o /tmp/todo
/tmp/todo
```

It prints `listening on 127.0.0.1:PORT`. Open `http://127.0.0.1:PORT/docs` in a browser and keep its developer console open: nothing should be logged there at any step.

## Steps and what to see

1. **The page.** The header says "API documentation", the title "Todo API", badges with `version 1.0.0` and `OpenAPI 3.1.0`, and links to `openapi.json` and `openapi.yaml`, which open the documents. Groups "App" and "default" list the eight operations, each with a coloured method badge; `GET /api/todos/{id}` shows its summary "Fetch one todo.".
2. **Expand and collapse.** Click an operation: its parameters, request body and responses appear, and the address bar gets `#op-<id>`. Click a group name: the group folds away. Reload with `#op-update` in the address: the page opens with `PATCH /api/todos/{id}` expanded and in view.
3. **Schemas.** In `PATCH /api/todos/{id}`, open `Update` under the request body and `Todo` under the 200 response: tables of fields with their types, `required`, and `default false` on `done`.
4. **Filter.** Type `health` in the filter: only `GET /health` stays, in the "default" group. Clear it: everything is back.
5. **Try it out, success.** Open `POST /api/todos`, press "Try it out": the body box is prefilled with `{"title": ""}`. Change it to `{"title": "check the page"}` and press "Send": status `201` in green, the time it took, the pretty-printed todo, the response headers, and a `curl` command that the "Copy" button copies.
6. **Try it out, errors.** `GET /api/todos/{id}` with `9`: `404` with `{"error": ...}`. With `abc`: `400`. `POST /api/todos` with `{"title": " "}`: `422`.
7. **Missing input.** `GET /api/todos/{id}` with the id empty and "Send": the page names the missing `id` and sends nothing.
8. **Models.** At the bottom, "Models" lists `Error`, `NewTodo`, `Todo` and `Update`; each opens to its table.
9. **Dark and light.** Switch the system (or the developer tools' emulated) colour scheme: the page follows, and every text stays readable.
10. **Phone width.** Narrow the window to about 375 pixels (or use device emulation): no sideways scrolling; the parameter tables drop their "In" column and the "Try it out" fields stack above their labels.
11. **Keyboard.** Tab through the page: every operation, button and field gets a visible focus ring, Enter or Space opens an operation, and each field's label is read with it.

## Last run

2026-11-20, Chromium in the desktop app's browser pane, against the todo service: every step as described, no console errors.
