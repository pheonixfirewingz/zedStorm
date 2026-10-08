# Mermaid diagram rendering

This optional extension renders Mermaid code blocks in Markdown previews and chat.
It uses the editor's colors and `mermaid_font_family` setting. Zoom, scrolling,
source viewing, and copying remain available in the Markdown interface.

In a development or nightly build of ZedStorm, run **zed: install dev extension**
from the command palette and select this directory (`extensions/mermaid`).
The extension uses the local, unreleased extension API v0.8.0 and requires a build
of ZedStorm containing the diagram-rendering API. Cargo and the
`wasm32-wasip2` Rust target are required; the development-extension installer
installs the target when necessary.

Uninstall the Mermaid extension to show Mermaid blocks as source code. Open
Markdown views refresh when the extension is installed, reloaded, or removed.

The `renderer` directory contains the Mermaid-to-SVG implementation and its
regression tests. It has no GPUI dependency and runs inside the extension's
WebAssembly sandbox. The editor parses and rasterizes the returned SVG.

The manifest declares `diagram_renderers = ["mermaid"]`. The extension implements
`Extension::render_diagram`, receiving the renderer ID, source, and a JSON theme.
The theme follows `MermaidTheme` in `renderer/src/mermaid_render.rs`; colors use
normalized `{ "h", "s", "l", "a" }` components. The extension computes readable
Git branch label colors and returns SVG or an error. This hook is part of the
unreleased v0.8.0 API; older extension API versions continue to work unchanged.

The extension has its own Cargo workspace and lockfile. Core ZedStorm does not
resolve or compile its rendering dependencies.

To check and test the extension from the repository root:

```sh
cargo check --manifest-path extensions/mermaid/Cargo.toml --target wasm32-wasip2
cargo test --manifest-path extensions/mermaid/Cargo.toml -p mermaid_render
```
