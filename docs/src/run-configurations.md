---
title: Run and Debug Configurations
description: Configure named Run and Debug targets and launch them from the title bar.
---

# Run and Debug Configurations

Use the title-bar configuration selector to choose what the adjacent **Run** and
**Debug** buttons launch. Selecting a configuration does not launch it.

## Create a configuration {#create-configuration}

1. Open a project folder.
2. Open the title-bar configuration selector and choose **Edit Configurations…**,
   or use {#action run::EditConfigurations} from the command palette.
3. Click **Add**. For a Cargo project with one clear binary target, the editor
   supplies its name and Run command. If an executable has already been built,
   it also supplies the Debug target, CodeLLDB adapter, and program path.
4. Review the targets and click **Save**, **Run**, or **Debug**.

Cargo detection respects the opened package, workspace default members,
`default-run`, and the output directory reported by Cargo. When both debug and
release binaries exist, it selects the most recently modified executable. It
never chooses an arbitrary binary in an ambiguous workspace. Existing saved
configurations keep their settings.

The basic form shows the command and any known executable. Empty optional fields
are hidden. **Choose executable…** opens the program setting when no executable
was detected. **Advanced…** exposes working directory, arguments, environment
variables, sharing, multiple instances, debugger selection, attach mode, and
adapter-specific JSON options. Configured arguments and environment variables
have a shortcut to their advanced settings. Enter each argument in its own row;
an argument containing spaces remains a single argument.

Enable **Attach to a process** under Advanced to configure an attach request;
enter the adapter's process identifier or connection options in the additional
adapter options.

Run and Debug targets are independent. A shell command becomes debuggable only
when you configure a Debug target. The title-bar Debug button is unavailable when
the selected adapter is unavailable.

## Edit and save {#edit-and-save}

The editor groups configurations by target type and supports search, duplication,
and removal. Switching between configurations keeps your unsaved edits.

- **Apply** saves all edited configurations and keeps the editor open.
- **Save** saves all edited configurations and closes the editor.
- **Cancel** discards edits since the last successful save.
- **Run** or **Debug** saves the edited configurations and launches the selected
  one in that mode.

Renaming a configuration preserves its identity and selection. Removing a
configuration does not stop sessions that are already running.

If a configuration changes outside the editor, saving reports a conflict and
keeps your draft. Reopen **Edit Configurations…** to load the external changes.

## Local and shared configurations {#configuration-storage}

Under **Advanced…**, enable **Share with project** to save a configuration in `.zed/run.json`.
Otherwise, the configuration remains local in ZedStorm's project data. You can
change this choice for an existing configuration.

Shared configurations use version 1 of the following format:

```json
{
  "version": 1,
  "configurations": {
    "api-server": {
      "name": "API server",
      "run": {
        "label": "API server",
        "command": "cargo",
        "args": ["run", "--package", "api"],
        "cwd": "$ZED_WORKTREE_ROOT"
      },
      "debug": {
        "label": "API server",
        "adapter": "CodeLLDB",
        "request": "launch",
        "program": "$ZED_WORKTREE_ROOT/target/debug/api",
        "cwd": "$ZED_WORKTREE_ROOT"
      }
    }
  }
}
```

The map key is the configuration's stable identifier. The editor generates it
when you add a configuration. Keep that key when renaming a configuration in
the file. Comments are supported and preserved when the editor updates fields.

This workflow reads `.zed/run.json` and saved local configurations. It does not
import `tasks.json` or `debug.json`.

## Launch and sessions {#launch-and-sessions}

Choose a configuration, then click **Run** or **Debug**, or use
{#action run::RunSelected} or {#action run::DebugSelected}. Each launch reloads
the configuration and resolves project/editor variables in its project root.

Run output uses a task-backed terminal. Debugging uses the existing Debug panel,
including session tabs and splits. The title-bar configuration menu lists running debug sessions at the top to switch between them, and **Stop** acts on the active debug session.

The selected configuration is remembered for each project. Opening another
project restores its selection without launching any processes.

The current configuration editor requires local projects. Compound configurations
and a combined Run/Debug session dashboard are not yet available.

See [Tasks](./tasks.md) for task variables and terminal execution options and
[Debugger](./debugger.md) for debugging controls and supported adapters.
