mod tools_path;

use std::{env, fs, path::PathBuf};
use zed::settings::LspSettings;
use zed_extension_api::{self as zed, LanguageServerId, Result};

const BINARY_NAME: &str = "yaml-language-server";

struct YamlExtension {
    cached_binary_path: Option<String>,
}

impl YamlExtension {
    fn binary_path(&mut self, worktree: &zed::Worktree) -> Result<String> {
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|stat| stat.is_file()) {
                return Ok(path.clone());
            }
        }

        let (os, _arch) = zed::current_platform();
        let binary_filename = if os == zed::Os::Windows {
            format!("{BINARY_NAME}.exe")
        } else {
            BINARY_NAME.to_string()
        };

        // 1. Check local extension bundle's bin/ directory
        if let Ok(current_dir) = env::current_dir() {
            let local_bin = current_dir.join("bin").join(&binary_filename);
            if fs::metadata(&local_bin).is_ok_and(|stat| stat.is_file()) {
                let path = local_bin.to_string_lossy().to_string();
                self.cached_binary_path = Some(path.clone());
                return Ok(path);
            }
        }

        // 2. Check zedstorm-tools managed tools directory
        let env_vars = worktree.shell_env();
        let os_str = match os {
            zed::Os::Mac => "mac",
            zed::Os::Linux => "linux",
            zed::Os::Windows => "windows",
        };

        if let Ok(tools_dir) = tools_path::extension_directory(os_str, &env_vars) {
            let tools_bin = tools_dir.join("bin").join(&binary_filename);
            if fs::metadata(&tools_bin).is_ok_and(|stat| stat.is_file()) {
                let path = tools_bin.to_string_lossy().to_string();
                self.cached_binary_path = Some(path.clone());
                return Ok(path);
            }
        }

        // 3. Check system PATH
        if let Some(path) = worktree.which(BINARY_NAME) {
            self.cached_binary_path = Some(path.clone());
            return Ok(path);
        }

        // Fallback to relative bin path in extension dir
        let fallback = PathBuf::from("bin").join(&binary_filename);
        let path = fallback.to_string_lossy().to_string();
        self.cached_binary_path = Some(path.clone());
        Ok(path)
    }
}

impl zed::Extension for YamlExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let path = self.binary_path(worktree)?;

        Ok(zed::Command {
            command: path,
            args: vec!["--stdio".to_string()],
            env: worktree.shell_env(),
        })
    }

    fn language_server_workspace_configuration(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        LspSettings::for_worktree(server_id.as_ref(), worktree)
            .map(|lsp_settings| lsp_settings.settings)
    }

    fn language_server_initialization_options(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        LspSettings::for_worktree(server_id.as_ref(), worktree)
            .map(|lsp_settings| lsp_settings.initialization_options)
    }
}

zed::register_extension!(YamlExtension);
