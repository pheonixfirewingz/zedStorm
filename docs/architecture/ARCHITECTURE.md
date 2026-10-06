# Codex chat integration

ZedStorm provides a native GPUI chat panel backed by the independently installed Codex CLI. Codex owns authentication, model selection, configuration, permissions, and persisted conversation data. The editor displays the conversation and forwards user input and approval decisions.

`zed::OpenCodex` opens the right-hand dock from the title bar, View menu, command palette, and Ctrl+Alt+J (Cmd+Alt+J on macOS). The panel starts `codex app-server` only when activated. Its working directory is the first worktree root, the parent of a single-file worktree, or the user's home directory when no project is open. Remote projects show an unsupported-project message instead of running commands against unrelated local files.

The connection uses newline-delimited JSON over child-process stdin/stdout. A background task owns the process and drains stderr, while a foreground task updates GPUI entities. Dropping the panel cancels those tasks and terminates its child process. New Chat creates a fresh process and thread; Reconnect resumes the current thread and reloads its history from Codex. The panel retains its current thread ID during the editor session; conversation selection across editor restarts is not implemented.

`codex_protocol.rs` tracks requests, active thread/turn IDs, streamed items, authentication, and pending approval requests. `codex_panel.rs` renders Markdown replies, project/model status, command and file-change activity, a multiline composer, errors, and request controls. Enter sends and Shift+Enter inserts a newline. Stop handles both active turns and a click during thread/turn startup. Server requests are scoped to the active conversation, answered only through user controls, and removed when resolved or interrupted. Unsupported server requests receive an explicit error instead of leaving the agent waiting indefinitely.

The integration inherits the CLI's model, sandbox, and approval configuration. It reuses existing CLI credentials; when required, the panel starts Codex's browser login. Zed account authentication, upstream agent/model-provider initialization, edit predictions, collaboration panels, and their settings remain disabled. Shared editor libraries retain upstream types and test infrastructure.

See [the integration decision](adr/0001-codex-cli.md), [the component diagram](codex-cli.mmd), and [the Codex App Server documentation](https://learn.chatgpt.com/docs/app-server).
