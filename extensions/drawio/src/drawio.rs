#[allow(dead_code)]
mod tools_path;

use zed_extension_api::{self as zed, TabRendererBackend, TabRendererWebEngine, Worktree};

struct DrawioExtension;

impl zed::Extension for DrawioExtension {
    fn new() -> Self {
        Self
    }

    fn tab_renderer_backend(
        &mut self,
        _renderer_id: &str,
        _file_path: &str,
        _worktree: &Worktree,
    ) -> Result<TabRendererBackend, String> {
        Ok(TabRendererBackend::WebEngine(TabRendererWebEngine {
            url: "https://embed.diagrams.net/?embed=1&proto=json".to_string(),
            html: None,
        }))
    }
}

zed::register_extension!(DrawioExtension);
