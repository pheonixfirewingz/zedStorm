mod tools_path;

use std::env;
use zed_extension_api::{
    self as zed, current_platform, Command, Os, TabRendererBackend, Worktree,
};

struct DrawioExtension;

impl zed::Extension for DrawioExtension {
    fn new() -> Self {
        Self
    }

    fn tab_renderer_backend(
        &mut self,
        _renderer_id: &str,
        file_path: &str,
        _worktree: &Worktree,
    ) -> Result<TabRendererBackend, String> {
        let os = match current_platform().0 {
            Os::Windows => "windows",
            Os::Mac => "mac",
            Os::Linux => "linux",
        };
        let binary_name = if os == "windows" {
            "zed-diagrams.exe"
        } else {
            "zed-diagrams"
        };
        // 1. Check local extension bin/ directory
        if let Ok(current_dir) = env::current_dir() {
            let local_bin = current_dir.join("bin").join(binary_name);
            if local_bin.is_file() {
                return Ok(TabRendererBackend::Command(Command {
                    command: local_bin.to_string_lossy().to_string(),
                    args: vec!["--embedded".to_string(), file_path.to_string()],
                    env: Vec::new(),
                }));
            }
        }

        // 2. Check installed extension directory
        let env_vars: Vec<(String, String)> = env::vars().collect();
        if let Ok(tools_dir) = tools_path::extension_directory(os, &env_vars) {
            let companion = tools_dir.join("bin").join(binary_name);
            if companion.is_file() {
                return Ok(TabRendererBackend::Command(Command {
                    command: companion.to_string_lossy().to_string(),
                    args: vec!["--embedded".to_string(), file_path.to_string()],
                    env: Vec::new(),
                }));
            }
        }

        // 3. Fallback: relative path in bin/ (resolved against installed extension by host)
        Ok(TabRendererBackend::Command(Command {
            command: format!("bin/{binary_name}"),
            args: vec!["--embedded".to_string(), file_path.to_string()],
            env: Vec::new(),
        }))
    }
}

zed::register_extension!(DrawioExtension);
