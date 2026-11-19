# Editor support

| Directory | Editor | Gives |
|---|---|---|
| `zed/` | Zed | Highlighting, outline, indentation, bracket matching, text objects, run buttons on `main` and `@test` functions, and the language server started automatically |
| `vscode/` | VS Code, and editors that read TextMate grammars (Sublime Text, GitHub Linguist) | Highlighting, comment toggling, bracket matching, indentation after `:`, folding |
| `vim/` | Vim and Neovim | Highlighting and file type detection |
| `tree-sitter-weby/` | Anything that uses Tree-sitter (Zed, Neovim, Helix) | The grammar itself, with highlight queries |

**Zed:** run `editors/zed/install-local.sh`, then in Zed open the command palette, choose *zed: install dev extension*, and pick the directory the script printed (`~/.local/share/weby-zed` by default). Zed builds the grammar and the extension itself, which needs Rust installed through `rustup`. The script exists because Zed fetches the grammar from a git repository at a fixed commit, and this repository is private: it points the copy at your checkout's current commit, so commit grammar changes before running it. Once the repository is public, installing straight from `editors/zed` works.

The extension starts `weby lsp` using `weby` from `PATH`, or the binary set in Zed's settings:

```json
{ "lsp": { "weby": { "binary": { "path": "/path/to/new_language/target/release/weby" } } } }
```

Its tasks (`task: spawn`) run, test or check the current file, and the ▶ beside `def main()` or a `@test` function runs that program or that test.

**VS Code:** copy or link `vscode/` into `~/.vscode/extensions/weby-language`, then reload.

**Vim / Neovim:** copy `vim/ftdetect` and `vim/syntax` into `~/.vim/` (or `~/.config/nvim/`).

## The grammar

`tree-sitter-weby/` is a Tree-sitter grammar written from the compiler's parser and the grammar sketch in [docs/01](../docs/01-language-design.md#grammar-sketch). `src/scanner.c` produces the indentation tokens the same way the compiler's lexer does; `src/parser.c` is generated, and committed because editors build from it. After changing `grammar.js`:

```bash
cd editors/tree-sitter-weby
tree-sitter generate --abi 14
tree-sitter test
```

`test/corpus/` holds the expected trees. Every `.wb` file in the repository, and the standard library, parses without errors; of the compiler's error tests in `tests/ui/`, only the four with syntax errors do not. The grammar accepts a little more than the compiler (statements at the top level, comparison chains) so that a file being edited keeps its highlighting. `queries/highlights.scm` is the same file as Zed's, in the order Zed and Neovim use (later patterns win).

## The language server

`weby lsp` speaks the Language Server Protocol on standard input and output: diagnostics as you type, hover for the type of an expression, go to definition, and formatting with `weby fmt`'s layout. Zed starts it through the extension. Any editor with a generic LSP client can use it. For Neovim 0.11:

```lua
vim.lsp.config("weby", { cmd = { "weby", "lsp" }, filetypes = { "weby" }, root_markers = { "weby.toml", ".git" } })
vim.lsp.enable("weby")
```

For Helix, in `languages.toml`:

```toml
[language-server.weby]
command = "weby"
args = ["lsp"]

[[language]]
name = "weby"
scope = "source.weby"
file-types = ["wb"]
language-servers = ["weby"]
comment-token = "#"
indent = { tab-width = 4, unit = "    " }
```

VS Code needs a small client extension to start a language server; one is not included yet, since building it needs packages from npm.
