//! Finding, reading and parsing the modules a program imports (docs/03, "Module graph").
//!
//! `import a.b` names the file `a/b.wb` under the directory of the program's main file (the
//! project root), and failing that, the standard library module `a.b`. Modules are loaded
//! depth-first; a module that imports one of the modules still being loaded closes a cycle,
//! which is an error.

use lang_diag::{Diagnostic, Diagnostics, SourceMap, Span, codes};
use lang_names::SOURCE_EXTENSION;
use lang_syntax::ast;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub struct LoadedModule {
    pub name: String,
    pub ast: ast::Module,
    pub stdlib: bool,
}

enum State {
    /// Being loaded: its imports are being followed. The index is its place in `stack`.
    Loading,
    Done,
}

struct Loader<'a> {
    root: PathBuf,
    sources: &'a mut SourceMap,
    diags: &'a mut Diagnostics,
    states: HashMap<String, State>,
    /// Module names from the main file to the one being loaded, for describing cycles.
    stack: Vec<String>,
    /// Modules in the order they finished loading: each after everything it imports.
    order: Vec<LoadedModule>,
}

/// Load every module `main` imports, directly or not. Returns them with their dependencies
/// first and `main` last.
pub fn load(
    root: &Path,
    main_name: String,
    main: ast::Module,
    sources: &mut SourceMap,
    diags: &mut Diagnostics,
) -> Vec<LoadedModule> {
    let mut loader = Loader {
        root: root.to_path_buf(),
        sources,
        diags,
        states: HashMap::new(),
        stack: Vec::new(),
        order: Vec::new(),
    };
    loader.visit(main_name, main, false);
    loader.order
}

impl Loader<'_> {
    fn visit(&mut self, name: String, module: ast::Module, stdlib: bool) {
        self.states.insert(name.clone(), State::Loading);
        self.stack.push(name.clone());
        for item in &module.items {
            if let ast::Item::Import(import) = item {
                self.follow(import);
            }
        }
        self.stack.pop();
        self.states.insert(name.clone(), State::Done);
        self.order.push(LoadedModule { name, ast: module, stdlib });
    }

    fn follow(&mut self, import: &ast::ImportDecl) {
        let name: String = import.path.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(".");
        let span: Span = import.path[0].span.to(import.path.last().expect("a path").span);
        match self.states.get(&name) {
            Some(State::Done) => return,
            Some(State::Loading) if self.stack.len() == 1 && lang_sema::stdlib::source(&name).is_some() => {
                self.diags.push(
                    Diagnostic::error(
                        codes::IMPORT_CYCLE,
                        format!("this file is named `{name}`, which hides the standard library module `{name}`"),
                        span,
                        "imports the file itself",
                    )
                    .with_help("rename the file"),
                );
                return;
            }
            Some(State::Loading) => {
                let start = self.stack.iter().position(|n| *n == name).unwrap_or(0);
                let mut cycle: Vec<&str> = self.stack[start..].iter().map(String::as_str).collect();
                cycle.push(&name);
                self.diags.push(
                    Diagnostic::error(
                        codes::IMPORT_CYCLE,
                        format!("modules import each other: {}", cycle.join(" -> ")),
                        span,
                        "this import closes the cycle",
                    )
                    .with_help("move what both modules need into a third module that both import"),
                );
                return;
            }
            None => {}
        }
        let relative: PathBuf = import.path.iter().map(|p| p.name.as_str()).collect::<PathBuf>();
        let path = self.root.join(relative).with_extension(SOURCE_EXTENSION);
        let mut stdlib = false;
        let (display, text) = if path.is_file() {
            match fs::read_to_string(&path) {
                Ok(text) => (path.display().to_string(), text),
                Err(error) => {
                    self.diags.push(Diagnostic::error(
                        codes::UNKNOWN_MODULE,
                        format!("cannot read module `{name}` from `{}`: {error}", path.display()),
                        span,
                        "could not be read",
                    ));
                    return;
                }
            }
        } else if let Some(source) = lang_sema::stdlib::source(&name) {
            stdlib = true;
            (lang_sema::stdlib::display_name(&name), source.to_string())
        } else {
            self.diags.push(
                Diagnostic::error(
                    codes::UNKNOWN_MODULE,
                    format!("cannot find module `{name}`"),
                    span,
                    "no such module",
                )
                .with_help(format!(
                    "looked for `{}` and for a standard library module; the standard library has {}",
                    path.display(),
                    lang_sema::stdlib::MODULES.iter().map(|(n, _)| format!("`{n}`")).collect::<Vec<_>>().join(", ")
                )),
            );
            return;
        };
        let file = self.sources.add(display, text);
        let tokens = lang_syntax::lex(file, &self.sources.get(file).text, self.diags);
        let module = lang_syntax::parse(&tokens, self.diags);
        self.visit(name, module, stdlib);
    }
}
