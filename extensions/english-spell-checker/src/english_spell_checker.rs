mod tools_path;

use std::env;
use zed_extension_api::{
    self as zed, current_platform, Command, LanguageServerId, Os, Result, Worktree,
};

const BINARY_NAME: &str = "english-spell-checker";

struct EnglishSpellCheckerExtension;

impl zed::Extension for EnglishSpellCheckerExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let (platform, _) = current_platform();
        let os = match platform {
            Os::Windows => "windows",
            Os::Mac => "mac",
            Os::Linux => "linux",
        };
        let binary_file_name = if os == "windows" {
            format!("{BINARY_NAME}.exe")
        } else {
            BINARY_NAME.to_string()
        };

        // 1. Check local extension bin/ directory
        if let Ok(current_dir) = env::current_dir() {
            let local_bin = current_dir.join("bin").join(&binary_file_name);
            if local_bin.is_file() {
                return Ok(Command {
                    command: local_bin.to_string_lossy().to_string(),
                    args: Vec::new(),
                    env: worktree.shell_env(),
                });
            }
        }

        // 2. Check managed tools directory (zedstorm-tools)
        let env_vars: Vec<(String, String)> = env::vars().collect();
        if let Ok(tools_dir) = tools_path::extension_directory(os, &env_vars) {
            let companion = tools_dir.join("bin").join(&binary_file_name);
            if companion.is_file() {
                return Ok(Command {
                    command: companion.to_string_lossy().to_string(),
                    args: Vec::new(),
                    env: worktree.shell_env(),
                });
            }
        }

        // 3. Check system PATH
        if let Some(path) = worktree.which(BINARY_NAME) {
            return Ok(Command {
                command: path,
                args: Vec::new(),
                env: worktree.shell_env(),
            });
        }

        // Fallback: relative bin path in the extension directory
        Ok(Command {
            command: format!("bin/{binary_file_name}"),
            args: Vec::new(),
            env: worktree.shell_env(),
        })
    }
}

zed::register_extension!(EnglishSpellCheckerExtension);
