//! Starts `weby lsp` for Weby files. The binary is the one named in Zed's settings
//! (`lsp.weby.binary.path`, with optional `arguments`), or else `weby` from the project's `PATH`.

use zed_extension_api::settings::LspSettings;
use zed_extension_api::{self as zed, LanguageServerId, Result};

struct WebyExtension;

impl zed::Extension for WebyExtension {
    fn new() -> Self {
        WebyExtension
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let binary =
            LspSettings::for_worktree(language_server_id.as_ref(), worktree).ok().and_then(|settings| settings.binary);
        let configured_path = binary.as_ref().and_then(|b| b.path.clone());
        let command = match configured_path {
            Some(path) => path,
            None => worktree.which("weby").ok_or_else(|| {
                "`weby` is not on PATH. Build it with `cargo build --release` in the Weby repository and add \
                 target/release to PATH, or set `lsp.weby.binary.path` in Zed's settings."
                    .to_string()
            })?,
        };
        let args = binary.and_then(|b| b.arguments).unwrap_or_else(|| vec!["lsp".to_string()]);
        Ok(zed::Command { command, args, env: worktree.shell_env() })
    }
}

zed::register_extension!(WebyExtension);
