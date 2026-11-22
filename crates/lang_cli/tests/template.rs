//! `template.load_dir(..., reload=True)` and `http.render` over a real socket (docs/14,
//! milestone 16): templates in nested directories extend and include each other; an edit shows
//! on the next request, a new file can be rendered, a broken edit is a 500 whose details stay in
//! the log, and fixing it brings the page back. Dotfiles are not templates.

mod common;

use common::{build_runtime, connect, read_response, start_with_env};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    // File times have a granularity; make sure an edit is seen as one.
    std::thread::sleep(Duration::from_millis(30));
}

fn get(port: u16, path: &str) -> common::Response {
    let (mut stream, mut reader) = connect(port);
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
    read_response(&mut reader, false)
}

#[test]
fn templates_reload_when_their_files_change() {
    build_runtime();
    let dir = std::env::temp_dir().join(format!("template-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write(
        &dir,
        "base.html",
        "<html><title>{% block title %}Shop{% endblock %}</title><body>{% block body %}{% endblock %}</body></html>",
    );
    write(&dir, "partials/item.html", "<li>{{ item.name }}: {{ item.price }}</li>");
    write(
        &dir,
        "shop.html",
        "{% extends \"base.html\" %}{% block title %}{{ title }}{% endblock %}\
         {% block body %}<ul>{% for item in items %}{% include \"partials/item.html\" %}{% endfor %}</ul>{% endblock %}",
    );
    write(&dir, ".hidden.html", "{{ never }}");
    let mut server = start_with_env("template_server", &[("TEMPLATES", dir.to_str().unwrap())]);

    let page = get(server.port, "/");
    assert_eq!(page.status, 200);
    assert_eq!(page.header("content-type"), Some("text/html; charset=utf-8"));
    assert_eq!(
        page.body,
        "<html><title>Corner &lt;shop&gt;</title><body><ul><li>tea: 3.5</li><li>jam &amp; toast: 4.25</li></ul></body></html>"
    );
    assert_eq!(get(server.port, "/names").body, "base.html, partials/item.html, shop.html");

    // An edit to an included file shows on the next request.
    write(&dir, "partials/item.html", "<li><b>{{ item.name }}</b></li>");
    assert!(get(server.port, "/").body.contains("<li><b>tea</b></li><li><b>jam &amp; toast</b></li>"));

    // A new file is a template at once.
    write(&dir, "extra.html", "x");
    assert_eq!(get(server.port, "/names").body, "base.html, extra.html, partials/item.html, shop.html");

    // A broken edit is a 500 without the details, which are in the log.
    write(&dir, "partials/item.html", "<li>{{ item.nope }}</li>");
    let broken = get(server.port, "/");
    assert_eq!(broken.status, 500);
    assert_eq!(broken.body, "{\"error\":\"internal server error\"}");
    assert!(!broken.body.contains("item.nope"));
    // A load error (here an include of a file that is gone) is a 500 as well.
    write(&dir, "shop.html", "{% include \"gone.html\" %}");
    assert_eq!(get(server.port, "/").status, 500);

    // Fixing the files brings the page back.
    write(&dir, "shop.html", "{{ title }}");
    write(&dir, "partials/item.html", "{{ item.name }}");
    let fixed = get(server.port, "/");
    assert_eq!((fixed.status, fixed.body.as_str()), (200, "Corner &lt;shop&gt;"));

    server.stop("template_server");
    let log = server.stderr.lock().unwrap().clone();
    assert!(log.contains("partials/item.html:1: `item.nope`"), "{log}");
    assert!(log.contains("shop.html:1: it includes `gone.html`, which does not exist"), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The example in `examples/recipes` builds, and its tests pass.
#[test]
fn the_recipes_example_passes_its_tests() {
    use lang_names::SOURCE_EXTENSION;
    build_runtime();
    let example = common::repo_root().join("examples/recipes");
    let check = std::process::Command::new(common::compiler())
        .arg("check")
        .arg(format!("main.{SOURCE_EXTENSION}"))
        .current_dir(&example)
        .output()
        .unwrap();
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
    let test = std::process::Command::new(common::compiler()).arg("test").current_dir(&example).output().unwrap();
    let stdout = String::from_utf8_lossy(&test.stdout);
    assert!(
        test.status.success() && stdout.contains("5 passed; 0 failed"),
        "{stdout}{}",
        String::from_utf8_lossy(&test.stderr)
    );
}
