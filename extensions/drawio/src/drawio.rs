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
        let args = vec!["--embedded".to_string(), file_path.to_string()];
        let env_vars: Vec<(String, String)> = env::vars().collect();
        if let Ok(extension_dir) = tools_path::extension_directory(os, &env_vars) {
            let companion = extension_dir.join("bin").join(binary_name);
            if companion.is_file() {
                return Ok(TabRendererBackend::Command(Command {
                    command: companion.to_string_lossy().to_string(),
                    args,
                    env: Vec::new(),
                }));
            }
        }

        // The host resolves relative commands against the installed extension directory,
        // which is where the release archive places the companion binaries.
        Ok(TabRendererBackend::Command(Command {
            command: format!("bin/{binary_name}"),
            args,
            env: Vec::new(),
        }))
    }
}

zed::register_extension!(DrawioExtension);
