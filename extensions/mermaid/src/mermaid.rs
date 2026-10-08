use zed_extension_api::{self as zed, Extension, Result};

struct MermaidExtension;

impl Extension for MermaidExtension {
    fn new() -> Self {
        Self
    }

    fn render_diagram(&mut self, renderer_id: &str, source: &str, theme: &str) -> Result<String> {
        if renderer_id != "mermaid" {
            return Err(format!("Unsupported diagram renderer: {renderer_id}"));
        }

        let mut theme: mermaid_render::MermaidTheme = serde_json::from_str(theme)
            .map_err(|error| format!("Invalid Mermaid theme: {error}"))?;
        theme.git_branch_label_colors = theme
            .git_branch_colors
            .map(mermaid_render::text_color_for_background);
        mermaid_render::render_to_svg(source, &theme).map_err(|error| format!("{error:#}"))
    }
}

zed::register_extension!(MermaidExtension);
