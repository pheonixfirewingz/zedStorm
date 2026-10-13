# Codex chat integration

ZedStorm provides a native GPUI chat panel backed by the embedded `codex-core` Rust library. Codex owns authentication, configuration, permission enforcement, and persisted conversation data. The editor displays the conversation, offers model, reasoning, and access selectors, and forwards user input and approval decisions.

`zed::OpenCodex` opens the right-hand dock from the title bar, View menu, command palette, and Ctrl+Alt+J (Cmd+Alt+J on macOS). The panel starts an in-process Codex runtime only when activated. Its working directory is the first worktree root, the parent of a single-file worktree, or the user's home directory when no project is open. Remote projects show an unsupported-project message instead of running commands against unrelated local files.

The connection uses in-memory channels through the upstream `codex-app-server` library, preserving its thread, authentication, and approval semantics on top of `codex-core`. Each connection owns a Tokio runtime on a dedicated thread. Dropping its GPUI task signals cancellation; the adapter shuts down the embedded server and runtime. Requests run concurrently with event consumption so approvals and Stop can be handled while another request is pending. New Chat creates a fresh runtime and thread; Reconnect resumes the current thread and reloads its history from Codex.

The `ai` crate owns the native panel, its session state, AI command filtering, and workspace action registration. The `codex_runtime` crate owns the embedded Codex lifecycle and protocol bridge without depending on GPUI. `zed` calls `ai::dispatch_codex_helpers` before starting threads, `ai::init` after command-palette initialization and `ai::init_workspace` during panel initialization. Its headless MCP entry point uses `ai::serve_context_mcp`, which re-exports the existing `context_mcp` server. The crate depends on shared editor, GPUI, and workspace infrastructure; it has no dependency on the application crate.

`crates/codex_runtime/src/session.rs` tracks requests, active thread/turn IDs, streamed items, authentication, and pending approval requests. `crates/ai/src/codex_panel.rs` renders Markdown replies, project/model status, command and file-change activity, a multiline composer, errors, and request controls. Enter sends and Shift+Enter inserts a newline. Stop handles both active turns and a click during thread/turn startup. Server requests are scoped to the active conversation, answered only through user controls, and removed when resolved or interrupted. Unsupported server requests receive an explicit error instead of leaving the agent waiting indefinitely.

The integration initially inherits Codex's model, sandbox, and approval configuration from `~/.codex/config.toml` (or `CODEX_HOME`). The composer selectors apply model, supported reasoning levels up to High, and access overrides to subsequent turns. Returning to configured settings restores the original configuration. New Chat preserves the user's selections. It reuses existing Codex credentials; when required, the panel starts Codex's browser login. Upstream Zed agent, model-provider, prediction, web-search, sandbox, MCP/ACP, and AI evaluation crates are deleted. Their dependencies and editor integrations are removed, while AI commit-message generation uses the same embedded runtime. Codex owns the AI runtime and server configuration. ZedStorm dispatches Codex sandbox, filesystem, and patch helper modes before normal startup, using its own executable. No Codex executable is installed, downloaded, or launched. Zed account authentication and collaboration panels remain disabled; shared collaboration libraries support editor infrastructure.

See [the crate-boundary decision](adr/0002-ai-crate.md), [the integration decision](adr/0005-codex-core.md), [the component diagram](codex-core.mmd), and [the Codex App Server documentation](https://learn.chatgpt.com/docs/app-server).

The built-in context MCP also serves language-server navigation. The stdio process
owns MCP framing, tool schemas, root validation, budgets, and filesystem access.
For semantic calls it discovers the running workspace's authenticated loopback
bridge through a root-specific endpoint file in the application data directory.
The `ai` crate owns the listener and dispatches requests on GPUI's foreground
executor, using the workspace's existing `Project` and language-server buffer
registration. No second language-server process is started by the MCP client.
Live-source reads and navigation observe unsaved editor buffers. Read-only buffer
retention is bounded; dirty buffers stay retained, and renames open affected files
in the workspace for review. Closing the workspace cancels the bridge and removes
its endpoint when it is still the current registration.

Semantic and filesystem ownership deliberately differ: filesystem reads access
disk; `read_buffer` accesses the editor. Semantic rename requires write opt-in in
both processes, validates source freshness and the complete edit's paths, rejects
resource operations, and pushes edits into editor undo history without saving.
Capability failures, startup/indexing errors and timeouts are returned to the
client. The bridge is local to a user/host; it is not a remote editor service.
See [the semantic bridge decision](adr/0004-semantic-mcp.md),
[the semantic component diagram](semantic-mcp.mmd), and
[the tool/client contract](../../crates/context_mcp/README.md).
