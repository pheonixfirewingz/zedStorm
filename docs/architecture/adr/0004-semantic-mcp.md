# Use the running editor's language servers for semantic MCP tools

Status: Accepted

The existing stdio context MCP starts before GPUI initializes and has no access
to the editor's project or unsaved buffers. Starting separate language servers
there would duplicate indexing and could return different results from the UI.

Each local project workspace now publishes an authenticated loopback bridge.
The stdio process discovers it by canonical root, sends one authenticated request
per connection, and receives a bounded tool result. The editor executes typed
language-server commands against its current project on GPUI's foreground
executor. Existing static/dynamic capability checks and request error propagation
are reused. Workspace symbol requests query supporting servers directly to
preserve server errors rather than silently treating failures as empty results.

This keeps the MCP transport usable by both the ZedStorm panel and external
stdio clients, including Codex providers hosted in T3 Code. It adds no network
service beyond loopback and keeps credentials in the user's application data
directory. Filesystem tools remain usable when the editor is closed. Semantic
tools require an open project; multiple windows for one root select the newest
published endpoint. Remote hosts and separate application data directories are
outside this connection contract.

Mutation authority is independent in the client and editor. Scoped symbol rename
checks source freshness, validates target paths before application, forbids file
resource operations and retains undoable unsaved buffers. A dropped connection
can obscure a completed mutation, so clients must inspect editor state before
retrying. This replaces navigation and rename workflows; Serena-specific memories
and arbitrary symbol-body editing are separate capabilities.
