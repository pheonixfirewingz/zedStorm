use zed::settings::LspSettings;
use zed_extension_api::{self as zed, Command, LanguageServerId, Result};

struct GoExtension;

impl zed::Extension for GoExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Command> {
        let lsp_settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree).ok();
        let binary_path = lsp_settings
            .as_ref()
            .and_then(|settings| settings.binary.as_ref())
            .and_then(|bin| bin.path.clone())
            .or_else(|| worktree.which("gopls"));

        let Some(command) = binary_path else {
            return Err("`gopls` language server not found on PATH. Install it using `go install golang.org/x/tools/gopls@latest` or configure its binary path in Zed settings.".to_string());
        };

        let args = lsp_settings
            .as_ref()
            .and_then(|settings| settings.binary.as_ref())
            .and_then(|bin| bin.arguments.clone())
            .unwrap_or_else(|| vec!["-mode=stdio".to_string()]);

        let env = lsp_settings
            .as_ref()
            .and_then(|settings| settings.binary.as_ref())
            .and_then(|bin| bin.env.clone())
            .map(|env| env.into_iter().collect())
            .unwrap_or_else(|| worktree.shell_env());

        Ok(Command { command, args, env })
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .map(|lsp_settings| lsp_settings.settings)
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        LspSettings::for_worktree(language_server_id.as_ref(), worktree)
            .map(|lsp_settings| lsp_settings.initialization_options)
    }
}

zed::register_extension!(GoExtension);
