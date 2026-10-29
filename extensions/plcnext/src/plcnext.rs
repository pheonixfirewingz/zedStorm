mod tools_path;

use std::env;
use zed_extension_api::{
    self as zed, current_platform, Command, Os, TabRendererBackend, Worktree,
};

const BINARY_NAME: &str = "zed-plcnext-viewer";

struct PlcNextExtension;

impl zed::Extension for PlcNextExtension {
    fn new() -> Self {
        Self
    }

    fn tab_renderer_backend(
        &mut self,
        _renderer_id: &str,
        file_path: &str,
        worktree: &Worktree,
    ) -> Result<TabRendererBackend, String> {
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

        // 1. Check local extension bin/ directory (e.g. during development)
        if let Ok(current_dir) = env::current_dir() {
            let local_bin = current_dir.join("bin").join(&binary_file_name);
            if local_bin.is_file() {
                return Ok(TabRendererBackend::Command(Command {
                    command: local_bin.to_string_lossy().to_string(),
                    args: vec!["--embedded".to_string(), file_path.to_string()],
                    env: worktree.shell_env(),
                }));
            }
        }

        // 2. Check managed tools directory (zedstorm-tools)
        let env_vars: Vec<(String, String)> = env::vars().collect();
        if let Ok(tools_dir) = tools_path::extension_directory(os, &env_vars) {
            let companion = tools_dir.join("bin").join(&binary_file_name);
            if companion.is_file() {
                return Ok(TabRendererBackend::Command(Command {
                    command: companion.to_string_lossy().to_string(),
                    args: vec!["--embedded".to_string(), file_path.to_string()],
                    env: worktree.shell_env(),
                }));
            }
        }

        // 3. Check system PATH
        if let Some(path) = worktree.which(BINARY_NAME) {
            return Ok(TabRendererBackend::Command(Command {
                command: path,
                args: vec!["--embedded".to_string(), file_path.to_string()],
                env: worktree.shell_env(),
            }));
        }

        // Fallback: relative path in bin/
        Ok(TabRendererBackend::Command(Command {
            command: format!("bin/{binary_file_name}"),
            args: vec!["--embedded".to_string(), file_path.to_string()],
            env: worktree.shell_env(),
        }))
    }
}

zed::register_extension!(PlcNextExtension);
