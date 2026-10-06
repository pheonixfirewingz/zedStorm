# Use Codex App Server for the integrated chat panel

Status: Accepted

ZedStorm needs a native chat interface backed by Codex CLI without Zed accounts or a second model-provider configuration flow. Run the installed `codex app-server` process and use its structured thread, turn, item, authentication, and approval messages over stdio.

The editor owns presentation and process lifecycle. Codex owns model requests, tool execution, configuration, credentials, and thread persistence. Approval and question requests are displayed in the panel and receive explicit user responses; existing Codex permissions are inherited without overrides.

This provides streamed Markdown replies, follow-up prompts, command/file activity, Stop, New Chat, and reconnection with history. A terminal session remains available through the editor's normal terminal, but the Codex title-bar button opens native chat.

The initial integration supports local projects and retains the active thread during the editor session. A thread picker across editor restarts and remote app-server transport are future work. Unknown server requests are rejected visibly rather than left pending.
