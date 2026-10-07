# Keep AI integration in a dedicated crate

Status: Accepted

## Context

The Codex panel and protocol lived inside the application crate. Adding model, reasoning, and access controls expanded that integration, while process management, command registration, and MCP setup remained coupled to application startup.

## Decision

Move the panel and protocol into `crates/ai`, with `src/ai.rs` as its library root. Expose application initialization, workspace initialization, and the headless context MCP entry point. Keep panel and protocol modules private. Keep `context_mcp` as the reusable server implementation behind the AI integration.

## Consequences

The application retains its command-line argument handling, menus, keymaps, and general command filtering. AI behavior and session ownership belong to `ai`, which can be checked independently of the application. The crate still depends on shared editor and workspace infrastructure because the native panel needs those services. Existing action names and behavior stay compatible.
