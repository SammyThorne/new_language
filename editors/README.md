# Editor support

| Directory | Editor | Gives |
|---|---|---|
| `vscode/` | VS Code, and editors that read TextMate grammars (Sublime Text, Zed, Helix through its grammar import, GitHub Linguist) | Highlighting, comment toggling, bracket matching, indentation after `:`, folding |
| `vim/` | Vim and Neovim | Highlighting and file type detection |

**VS Code:** copy or link `vscode/` into `~/.vscode/extensions/weby-language`, then reload.

**Vim / Neovim:** copy `vim/ftdetect` and `vim/syntax` into `~/.vim/` (or `~/.config/nvim/`).

## The language server

`weby lsp` speaks the Language Server Protocol on standard input and output: diagnostics as you type, hover for the type of an expression, go to definition, and formatting with `weby fmt`'s layout. Any editor with a generic LSP client can use it. For Neovim 0.11:

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
