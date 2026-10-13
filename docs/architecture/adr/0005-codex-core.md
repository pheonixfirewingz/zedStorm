# Embed codex-core

## Decision

Replace the installed/downloaded Codex CLI and `codex app-server` subprocess with
the upstream `codex-app-server` in-process interface backed by `codex-core`.
Chat and commit-message generation use the same adapter in the GPUI-independent
`codex_runtime` crate. Authentication,
configuration, thread persistence, tools, sandboxing, and approval decisions stay
owned by Codex. The native panel continues to consume its existing protocol
envelopes, now delivered through memory channels.

The Codex libraries use the compatibility fork at
[`6f67766cb5839183a870cc463cee8d652b3e52a2`](https://github.com/pheonixfirewingz/codex/commit/6f67766cb5839183a870cc463cee8d652b3e52a2),
based on upstream revision `90f00bd89ed3263b4d5993849cccb795f4330449`.
Update all Codex dependency revisions together. The workspace also adopts the
SQLite version and WebSocket forks required by that upstream revision.

Codex and the editor share the editor's existing Tree-sitter 0.27.0 revision,
`43623ec9bf0eaaf7113285c46e8a09018f181b18`. The fork changes Codex's workspace
parser dependency and adds three checked child-index conversions in shell-command
parsing. No parser or Codex package sources are vendored in ZedStorm. The exact
compatibility delta is recorded in
[`patches/codex-tree-sitter-0.27.patch`](../patches/codex-tree-sitter-0.27.patch).
Cargo's Tree-sitter language-handle patch keeps grammar types on the same source
as the parser.

When updating Codex, replay this patch onto the chosen upstream revision, run the
shell-command and apply-patch test suites, and pin the resulting fork commit in
all workspace Codex dependencies. Changes to the parser also require the language
suite and full application check. The pinned fork preserves the editor's query
APIs without a parser downgrade or native backport.

Keep the Rama error, macro, and utility crates at the versions in Codex's
lockfile. Its network proxy pins Rama alpha.4, whose transitive semver constraints
otherwise admit newer, incompatible releases.

## Lifecycle

Each connection runs on a dedicated Tokio runtime with cancellation tied to the
GPUI connection task. Startup failures reach the panel. The adapter consumes
events while requests are pending, allowing approval responses and interruption
to proceed. Cancellation and closed channels trigger bounded server shutdown;
dropping the runtime stops remaining tasks. Lost events produce an explicit
reconnect error so the panel does not silently display incomplete state.

ZedStorm initializes and dispatches Codex's sandbox, filesystem, patch, and
process helper modes before normal application startup. The helper alias guard
remains alive for the application lifetime. These modes re-enter the ZedStorm
executable; they do not depend on a Codex CLI binary. The built-in context MCP
remains a separate stdio helper in the same executable.

## Validation

The final pinned fork passes `cargo check -p ai -p zed -p extension_cli` and all
470 selected unit tests: 24 embedded runtime, 187 editor language, 192 Codex
shell-command, and 67 Codex apply-patch tests. Formatting for `codex_runtime`
and the compatibility source changes also passes.

Cargo does not run Git dependencies' dev-dependencies as part of this workspace.
The two upstream parser suites were run in an isolated test workspace with
source symlinks to the fork, the same parser revision, and ZedStorm's dependency
features. That test workspace is temporary and adds no source copies to the
repository.
