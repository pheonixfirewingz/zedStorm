# IDEA-style run and debug workflow for ZedStorm

Status: Configuration selector/editor implemented; session aggregation and
compound launches remain proposed.

The implementation direction was updated to require no backward compatibility.
The new workflow reads `.zed/run.json` and local editor persistence only, without
importing tasks.json/debug.json. See [the implemented workflow](../src/run-configurations.md).

Researched on 7 October 2026 against IntelliJ IDEA 2026.2 documentation and
JetBrains' published UI images. Repository findings below describe the baseline
before the configuration selector/editor increment, including its existing
title-bar and multi-session debugger changes.

## Recommendation

Make a named configuration the common entry point for Run and Debug. Each launch
creates a distinct session with its own state, output, and controls. Use the
existing task, terminal, and Debug Adapter Protocol (DAP) implementations beneath
that interface.

The first useful increment is a configuration selector and editor. A combined
session view follows once run sessions expose reliable lifecycle events. Compound
launches come after those foundations.

## What IDEA does

| Area                 | Observed IDEA behavior                                                                                                 | Design implication                                                       |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| Configurations       | Named startup settings; temporary configurations can become permanent, and project files can be shared.                | Configuration identity must survive repeated launches.                   |
| Run widget           | Selects configurations, offers Run/Debug, and provides restart/stop during execution.                                  | Selection determines the next launch.                                    |
| Configuration editor | Groups configurations by type and exposes creation, editing, removal, and editable templates.                          | Use a list and form with explicit persistence.                           |
| Advanced settings    | Application configurations expose additional options, including environment variables and multiple instances.          | Keep essential fields visible; reveal advanced fields on demand.         |
| Run output           | Named tabs, pinning, splits, rerun, and stop controls.                                                                 | Output belongs to a session and can outlive its process.                 |
| Debugging            | Session tabs contain frames, variables, watches, threads, and console; editor annotations follow the selected session. | Preserve one clearly active debug context.                               |
| Services             | Can aggregate selected configuration types and their run/debug views; this is opt-in.                                  | A dashboard is useful for several services, but need not be the default. |
| Multiple launches    | Compound configurations launch in parallel without guaranteed order; Before Launch tasks provide sequential execution. | Separate concurrent groups from prerequisites.                           |

Sources: [configurations](https://www.jetbrains.com/help/idea/run-debug-configuration.html),
[run widget](https://www.jetbrains.com/help/idea/guided-tour-around-the-user-interface.html#toolbar),
[configuration editor](https://www.jetbrains.com/help/idea/run-debug-configurations-dialog.html),
[application options](https://www.jetbrains.com/help/idea/run-debug-configuration-java-application.html),
[Run panel](https://www.jetbrains.com/help/idea/run-tool-window.html),
[Debug panel](https://www.jetbrains.com/help/idea/debug-tool-window.html),
[Services](https://www.jetbrains.com/help/idea/services-tool-window.html), and
[compound launches](https://www.jetbrains.com/help/idea/run-debug-multiple.html).

IDEA limits temporary configurations to five by default. Saving a temporary
configuration and sharing it as a project file are separate operations.
[Configuration lifecycle](https://www.jetbrains.com/help/idea/run-debug-configuration.html).

IDEA's published images show a header with configuration selection beside play
and debug controls, and a configuration dialog with a left-hand type tree,
right-hand editing area, and Run/Cancel/Apply/OK footer.
[Header reference](https://resources.jetbrains.com/help/img/idea/2026.2/new_ui_window_header.png),
[editor reference](https://resources.jetbrains.com/help/img/idea/2026.2/ij_runConfigMenu.png).

## Existing ZedStorm foundations and gaps

| Component                                                              | Verified foundation                                                                                                                        | Gap to address                                                                                                                                                                                     |
| ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| [TitleBar::render](../../crates/title_bar/src/title_bar.rs)            | Run/Debug buttons, selected debug-session label, debug-session dropdown, and Stop.                                                         | The dropdown selects sessions. Run opens the task picker; Debug opens the debug picker. Neither launches a persistent selected configuration. Stop handles debugging only.                         |
| [TaskTemplate](../../crates/task/src/task_template.rs)                 | Command, arguments, environment, working directory, save/reveal behavior, and concurrency policy.                                          | No common run/debug configuration identity or richer form editor.                                                                                                                                  |
| [DebugScenario](../../crates/task/src/debug_format.rs)                 | Adapter, label, build prerequisite, arbitrary adapter configuration, and TCP adapter connection.                                           | Separate from run definitions; labels alone cannot safely identify configurations.                                                                                                                 |
| [NewProcessModal](../../crates/debugger_ui/src/new_process_modal.rs)   | Task/configure/attach modes, adapter selection, launch setup, and save to debug.json.                                                      | ConfigureMode exposes program, working directory, stop-on-entry, and save toggles. Arguments/environment are parsed from the command on non-Windows systems rather than edited as separate fields. |
| [DebugPanel](../../crates/debugger_ui/src/debugger_panel.rs)           | Multiple sessions, active-session tracking, restart/stop controls, tabs/splits, scenario saving, and close confirmation for live sessions. | No equivalent index for ordinary run sessions.                                                                                                                                                     |
| [Terminal TaskState/TaskStatus](../../crates/terminal/src/terminal.rs) | Task-backed terminal, completion receiver, and running/completed/unknown status.                                                           | A shared session layer needs build/start/stop/error states and stable links to process and output ownership.                                                                                       |

Existing [task documentation](../src/tasks.md) describes local/global definitions,
oneshot tasks, variable resolution, and rerun behavior.
Existing [debugger documentation](../src/debugger.md) describes launch/attach,
build prerequisites, generated scenarios, and multi-session layouts.

The debugger already has much of the desired session UI. Reuse its panes and
views rather than replacing its debugger engine or discarding split layouts.

## Proposed interface

### Header and configuration selector

```text
Project / Branch       [ API server v ] [ Run ] [ Debug ]
```

The selector displays active running sessions at the top when running, followed by
saved configurations, and Edit Configurations. Show type and project-root subtitles
where names collide. Selecting an entry never launches it.

Run and Debug launch that selection directly. Current File resolves the existing
editor/gutter runnable into a temporary configuration; show an explanation when
there is no runnable. Retain the existing quick pickers as separate actions.

Show Debug only as available when a configuration has a supported debug target.
Attach configurations support Debug/Attach, not Run. An arbitrary shell command
must not be presented as debuggable merely because it is executable.

Selection belongs to the active workspace/project, not globally to the window.
Switching projects restores that project's selection. Selecting a different
session does not silently change the launch configuration.

### Configuration editor

```text
Run / Debug Configurations
+ Add  - Remove  Duplicate           Name: API server
Search configurations               Type: Shell command
                                    Project root: backend
Shell commands                      Command: cargo
  > API server                      Arguments: run --package api
Native / DAP                        Working directory: $ZED_WORKTREE_ROOT
    API debugger                    Environment: [ Edit variables... ]
Attach                              Debug target: [ Configure... ]
Compound                            Before launch: [ + Add task ]
    Full stack                      Storage: Local / Shared with project

Edit templates                      Modify options...
                                    Cancel  Apply  Save  Run v
```

Use a resizable GPUI modal with a searchable configuration tree and scrollable
form. At compact widths, switch between list and form through a Back control.
Keep action buttons visible independently of form scrolling.

Initial types: shell command, native executable/DAP launch, and attach. Add
language-specific templates only where existing task/debug providers support
them. A Rust template can reuse task-to-debug scenario generation rather than
inventing a second Cargo integration.

The basic editor shows the name, enabled launch modes, command, and known
executable. Project detection supplies useful defaults instead of empty fields.
Cargo metadata identifies the opened package or an unambiguous default binary;
the current implementation uses existing debug/release artifacts from Cargo's
output directory. It does not overwrite saved targets or guess in ambiguous
workspaces. Unknown executables have a short setup action instead of an empty
program field.

Working directory, arguments, environment, sharing, multiple instances, attach,
and raw adapter options use progressive disclosure through Advanced. Configured
arguments and environment have direct shortcuts to their settings. Further
advanced options can include save policy, output reveal behavior, and
stop-on-entry. Keep argument boundaries explicit; distinguish shell commands
from executable launches rather than converting between them by splitting text.

Provide an adapter-specific raw JSON section that preserves unknown fields.
Use available adapter schemas for field assistance where supported; do not assume
every adapter supplies a complete form schema.

Apply persists changes and keeps the editor open. Save persists and closes it.
Cancel discards edits since the last successful Apply. Run/Debug uses a validated
draft; if it differs from saved settings, create a temporary launch snapshot.
Saving must preserve unrelated JSON fields and surface file-write conflicts.

Report validation next to the offending field. Examples: missing adapter,
unresolved project variable, missing referenced build task, and invalid JSON.
Launching is disabled while required fields are invalid. Failure to persist leaves
the draft available for correction.

Temporary configurations have a visible Temporary label and a Save action.
Saved local configurations and shared configurations have distinct storage labels.
Initial retention: five inactive temporary configurations; active or explicitly
pinned entries are protected. Pinning a configuration, saving it, and pinning a
session's output are distinct operations.

### Run, Debug, and optional Services panels

```text
Run:   [ API server #2: Running ] [ Tests: Finished ]
       Restart  Stop  Pin  Clear  Wrap  Scroll to end
       Process output / terminal

Debug: [ API debugger: Paused ] [ Worker debugger: Running ]
       Resume/Pause  Step over  Step into  Step out  Restart  Stop
       Threads / Frames | Variables / Watches
       Console / Terminal
```

Keep Run and Debug as recognizable bottom-panel destinations. Reuse the existing
debug session tabs, splits, frames, variables, and console. Each run session owns
a task-backed terminal view; regular interactive terminals stay in Terminal.
Moving a view must retain its live entity and process.

Retain finished output until explicitly closed or replaced under a documented
reuse policy. Pin prevents replacement. An unpinned completed tab may be reused
for the next launch of that configuration; concurrent live runs always get
separate instances. Configuration deletion never implicitly terminates sessions.

For closing a live session, preserve the existing debugger confirmation pattern
and provide explicit Stop and Close or Cancel. Hiding a panel never stops a
process. Attach sessions use Detach by default; termination is a separate
capability-dependent action.

A later Services view lists configurations and child sessions, grouped by type
or compound group, beside the selected session's existing Run/Debug view. Offer
it as an opt-in layout for multi-service projects.

## Session and launch behavior

| State        | Visible behavior and actions                                                 |
| ------------ | ---------------------------------------------------------------------------- |
| Preparing    | Resolve configuration and variables; cancellation remains available.         |
| Building     | Show the current prerequisite and its output; Stop cancels preparation.      |
| Starting     | Show launch progress; Stop cancels the pending launch.                       |
| Running      | Show live output; Restart and Stop; Pause only when supported.               |
| Paused       | Debug context is selected explicitly; Resume and supported stepping actions. |
| Stopping     | Show pending termination; expose force termination only when supported.      |
| Finished     | Preserve output, exit information, duration, Rerun, and Close.               |
| Failed       | Show the failed stage and actionable error; preserve available output.       |
| Disconnected | Show lost adapter/process connection; do not claim successful completion.    |

Restart targets one session, waits for termination, then launches its captured
configuration snapshot again. Run/Debug from the header uses current settings and
fresh context. Offer Run Latest Configuration from a session when its source has
changed. Editing configurations never mutates an already running instance.

For new configurations, multiple instances are off. Starting one with an active
instance offers Restart Existing or Cancel; enabling multiple instances creates
a separately named session. Stop targets the chosen instance, not every
process with the same label.

Before Launch executes ordered finite tasks and waits for successful completion.
A failure prevents the main launch and reveals the failing output. Cancellation
must propagate through the active step and prevent later steps from starting.
Long-lived servers belong in compound groups; exit-based prerequisites cannot
provide service readiness.

Compound configurations launch children concurrently, record each child's state,
and expose Stop Group. Validate missing references and dependency cycles before
starting. Partial failure remains visible without automatically terminating
successful siblings. Stop Group stops only sessions created by that group, not
pre-existing independent instances. Readiness dependencies are a later feature.

## Architecture and persistence proposal

See [the component diagram](run-debug.mmd) and
[the proposed boundary decision](adr/0003-run-debug-configurations.md).

| Owner                                | Responsibility                                                                                               |
| ------------------------------------ | ------------------------------------------------------------------------------------------------------------ |
| `task`                               | Serializable configuration identity/metadata and reusable launch definitions, with no UI dependency.         |
| `project`                            | Configuration catalog: local/shared/temporary sources, validation, variable resolution, and source watching. |
| `workspace`                          | Selected configuration and a shared session registry/controller with executor interfaces.                    |
| `tasks_ui` / terminal executor       | Existing task resolution/spawn integration, run lifecycle observation, and run output views.                 |
| `debugger_ui` / DAP executor         | Existing debug startup, session lifecycle, capabilities, and debug views.                                    |
| `title_bar` and configuration editor | Observe the shared controller and dispatch configuration/session actions.                                    |

The controller does not depend on concrete UI crates: registration supplies
executor handles through an interface, extending the existing debugger-provider
pattern. GPUI foreground entities own UI state; background tasks handle I/O.
Use weak back-references to avoid ownership cycles and preserve process ownership
when moving views.

A configuration has a stable ID, name, type, source/project-root identity,
supported launch modes, target definitions, prerequisites, and presentation
policy. A session has a separate ID, configuration ID, launch snapshot, mode,
project identity, lifecycle state, executor handle, and output handle. Names are
display text, never unique keys.

Shared configurations live in version 1 of `.zed/run.json`; local configurations
and project-scoped selection live in editor persistence. Configuration IDs are
object keys, generated on creation and preserved through renames. No legacy
configuration catalog or migration is provided.

The initial catalog and editor live in `debugger_ui` and are owned by DebugPanel.
The title bar observes that entity. The broader workspace session controller
remains a later increment.
Generated templates must remain read-only until copied into a saved configuration.
Persist unresolved variables; launch snapshots capture resolved arguments and
context. Preserve arbitrary DAP fields and existing JSON comments when editing.
Removing a referenced task reports a configuration error rather than launching a
different similarly named task.

Restore selections and panel layout after editor restart. Do not automatically
relaunch processes or present persisted records as live sessions. Remote launch
support stays restricted to existing executor capabilities, with explicit errors
for unsupported project types.

## Delivery plan and acceptance criteria

1. **Shared selection and identity.** Load the new run.json/local catalog,
   replace the session-only header selector, persist selection per project, and
   launch supported modes directly. Verify duplicate names, project switching,
   unavailable variables, and capability-disabled actions.
2. **Configuration editor.** Add structured fields, temporary-to-saved promotion,
   local/shared storage, and raw adapter JSON. Verify round trips preserve unknown
   fields/comments, Apply/Cancel semantics, external edits, and write failures.
3. **Run session integration.** Expose terminal lifecycle through the shared
   session registry, add run session controls/output retention, and integrate the
   existing debugger. Verify stopping/restarting the selected instance, build
   failure/cancellation, pinned output, concurrent runs, attach detach behavior,
   and moving a live session between panes.
4. **Compounds and Services.** Add group launches, ordered prerequisites, and
   optional aggregation. Verify cycle detection, partial failures, group-scoped
   cancellation, and preservation of independent sessions.

Visual verification should cover wide and compact native windows, long names,
empty configurations, validation errors, several active sessions, and keyboard
access. Preserve visible focus, accessible control labels, status text beyond
color alone, and existing theme tokens/icons. Audit shortcut conflicts before
offering an optional IDEA keymap; do not silently replace existing bindings.

The largest implementation risks are reliable process lifecycle observation,
stable configuration identity across storage changes, and lossless form editing of arbitrary
adapter configurations. Prototype those boundaries before committing to a final
file schema or a larger Services layout.
