# Unify run/debug configuration selection while reusing executors

Status: Accepted for the configuration selector/editor increment

## Context

The title bar selects debug sessions, while Run and Debug open launch pickers.
Tasks and debug scenarios have separate persistence and execution paths. The
debugger already supports multiple session tabs and splits, and ordinary tasks
already run in task-backed terminals.

The requested IDEA-style workflow needs persistent named configurations, direct
launch actions, and explicit control over individual run/debug instances.

## Proposed decision

Introduce a project configuration catalog and a workspace session controller.
Keep configuration identity separate from session identity. Connect existing
terminal/task and DAP execution through registered interfaces; keep concrete UI
dependencies outside workspace.

Present a common configuration selector and editor, with Run and Debug enabled
according to target capabilities. Keep Run and Debug output destinations and
reuse existing debug views. Offer a Services aggregation later.

Use version 1 of `.zed/run.json` for shared configurations and project-scoped
editor persistence for local configurations and selection. Do not import or
adapt tasks.json/debug.json; the requested workflow has no backward compatibility
layer. Configuration IDs are map keys and remain stable when names change.

The first implementation keeps the catalog and editor in `debugger_ui`, owned by
the existing DebugPanel. The title bar observes that catalog. Move orchestration
behind a workspace executor interface when ordinary run sessions join the shared
session registry; the initial increment does not introduce that registry.

## Alternatives

- Expand the existing session dropdown: smaller change, but it cannot represent
  saved configurations before any session exists.
- Replace both executors: duplicates terminal and debugger behavior without
  resolving configuration identity or selection semantics.
- Immediately migrate all definitions: simplifies the new catalog but risks
  breaking existing workflows and losing adapter fields.

## Consequences

The initial work can focus on selection and editing. Session aggregation requires
reliable lifecycle events from terminal tasks. Shared entries need stable
identity and lossless persistence. Debugging remains limited
to targets supported by installed adapters/providers.

See [the researched design](../run-debug-design.md) and
[component diagram](../run-debug.mmd) for interactions, scope, and validation.
