use futures::StreamExt as _;
use std::{
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context as _, Result};
use collections::BTreeMap;
use editor::{Editor, EditorMode, SizingBehavior};
use futures::io::AsyncBufReadExt;
use gpui::{
    App, AsyncWindowContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement,
    ParentElement, Pixels, Render, Styled, Subscription, Task, WeakEntity, Window,
    actions, div, px,
};
use project::{Fs, Project};
use ui::{
    Color, FluentBuilder,
    IconButton, IconName, IconSize, Label, LabelSize, Tab, Tooltip, prelude::*,
};
use util::ResultExt;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

use crate::run_configurations::{ConfigurationKey, RunConfiguration};

const RUN_PANEL_KEY: &str = "RunPanel";

actions!(
    run_panel,
    [
        /// Toggles the run panel.
        Toggle,
        /// Toggles focus on the run panel.
        ToggleFocus,
        /// Reruns the active session.
        RerunActive,
        /// Stops the active session.
        StopActive,
        /// Clears output of the active session.
        ClearOutput,
    ]
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Starting,
    Running,
    Finished(i32),
    Failed(i32),
    Stopped,
    Error,
}

pub struct RunSession {
    pub id: String,
    pub name: String,
    pub icon: IconName,
    pub configuration_key: Option<ConfigurationKey>,
    pub configuration_snapshot: RunConfiguration,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub state: SessionState,
    pub editor: Entity<Editor>,
    pub scroll_to_end: bool,
    pub soft_wrap: bool,
    pub child_pid: Option<u32>,
    pub execution_task: Option<Task<()>>,
    pub link_ranges: Vec<(std::ops::Range<usize>, PathBuf, u32, Option<u32>)>,
}

pub struct RunPanel {
    _workspace: WeakEntity<Workspace>,
    _project: Entity<Project>,
    _fs: Arc<dyn Fs>,
    focus_handle: FocusHandle,
    pub(crate) sessions: Vec<RunSession>,
    pub(crate) active_index: Option<usize>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for RunPanel {}

impl Focusable for RunPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for RunPanel {
    fn persistent_name() -> &'static str {
        "RunPanel"
    }

    fn panel_key() -> &'static str {
        RUN_PANEL_KEY
    }

    fn position(&self, _window: &Window, _cx: &App) -> DockPosition {
        DockPosition::Bottom
    }

    fn position_is_valid(&self, _: DockPosition) -> bool {
        true
    }

    fn activation_priority(&self) -> u32 {
        6
    }

    fn set_position(&mut self, _position: DockPosition, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn default_size(&self, _window: &Window, _: &App) -> Pixels {
        px(300.)
    }

    fn icon(&self, _window: &Window, _cx: &App) -> Option<IconName> {
        Some(IconName::PlayFilled)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Run")
    }

    fn toggle_action(&self) -> Box<dyn gpui::Action> {
        Box::new(Toggle)
    }
}

impl RunPanel {
    pub fn new(
        workspace: &Workspace,
        _window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let workspace_handle = workspace.weak_handle();
        let project = workspace.project().clone();
        let fs = workspace.app_state().fs.clone();

        cx.new(|cx| Self {
            _workspace: workspace_handle,
            _project: project,
            _fs: fs,
            focus_handle: cx.focus_handle(),
            sessions: Vec::new(),
            active_index: None,
            _subscriptions: Vec::new(),
        })
    }

    pub fn load(
        workspace: WeakEntity<Workspace>,
        cx: &mut AsyncWindowContext,
    ) -> Task<Result<Entity<Self>>> {
        cx.spawn(async move |cx| {
            workspace.update_in(cx, |workspace, window, cx| {
                let panel = Self::new(workspace, window, cx);
                panel
            })
        })
    }

    pub fn sessions(&self) -> &[RunSession] {
        &self.sessions
    }

    pub fn active_session(&self) -> Option<&RunSession> {
        self.active_index.and_then(|idx| self.sessions.get(idx))
    }

    pub fn active_session_mut(&mut self) -> Option<&mut RunSession> {
        self.active_index.and_then(|idx| self.sessions.get_mut(idx))
    }

    pub fn is_running_configuration(&self, key: &ConfigurationKey) -> bool {
        self.sessions.iter().any(|s| {
            s.configuration_key.as_ref() == Some(key)
                && matches!(s.state, SessionState::Starting | SessionState::Running)
        })
    }

    pub fn stop_session_by_key(&mut self, key: &ConfigurationKey, cx: &mut Context<Self>) {
        for (_i, session) in self.sessions.iter_mut().enumerate() {
            if session.configuration_key.as_ref() == Some(key)
                && matches!(session.state, SessionState::Starting | SessionState::Running)
            {
                Self::terminate_session_process(session);
                session.state = SessionState::Stopped;
                Self::append_raw_to_editor(
                    &session.editor,
                    "\nProcess stopped by user.\n",
                    Some(Color::Error),
                    cx,
                );
            }
        }
        cx.notify();
    }

    pub fn stop_all(&mut self, cx: &mut Context<Self>) {
        for session in &mut self.sessions {
            if matches!(session.state, SessionState::Starting | SessionState::Running) {
                Self::terminate_session_process(session);
                session.state = SessionState::Stopped;
                Self::append_raw_to_editor(
                    &session.editor,
                    "\nProcess stopped by user.\n",
                    Some(Color::Error),
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn terminate_session_process(session: &mut RunSession) {
        if let Some(task) = session.execution_task.take() {
            task.detach();
        }
        if let Some(pid) = session.child_pid.take() {
            #[cfg(unix)]
            {
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", "--", &format!("-{pid}")])
                    .output();
            }
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .output();
            }
        }
        if let Some(task) = session.execution_task.take() {
            task.detach();
        }
    }

    pub fn launch(
        &mut self,
        name: String,
        icon: IconName,
        key: Option<ConfigurationKey>,
        configuration: RunConfiguration,
        command: String,
        args: Vec<String>,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Enforce instance restriction if allow_multiple_instances is false
        if !configuration.allow_multiple_instances {
            if let Some(key_ref) = &key {
                let existing_index = self.sessions.iter().position(|s| {
                    s.configuration_key.as_ref() == Some(key_ref)
                        && matches!(s.state, SessionState::Starting | SessionState::Running)
                });
                if let Some(idx) = existing_index {
                    // Stop existing instance and restart it in the same tab
                    let session = &mut self.sessions[idx];
                    Self::terminate_session_process(session);
                    session.state = SessionState::Starting;
                    session.command = command.clone();
                    session.args = args.clone();
                    session.cwd = cwd.clone();
                    session.env = env.clone();
                    session.configuration_snapshot = configuration.clone();
                    self.active_index = Some(idx);
                    Self::clear_editor(&session.editor, cx);
                    self.spawn_process(idx, command, args, cwd, env, window, cx);
                    cx.notify();
                    return;
                }
            }
        }

        let editor = cx.new(|cx| {
            let mut editor = Editor::multi_line(window, cx);
            editor.set_mode(EditorMode::Full {
                scale_ui_elements_with_buffer_font_size: true,
                show_active_line_background: false,
                sizing_behavior: SizingBehavior::ExcludeOverscrollMargin,
            });
            editor.move_to_end(&editor::actions::MoveToEnd, window, cx);
            editor.set_read_only(true);
            editor.set_show_gutter(false, cx);
            editor.set_show_runnables(false, cx);
            editor.set_show_line_numbers(false, cx);
            editor.set_show_git_diff_gutter(false, cx);
            editor.set_autoindent(false);
            editor.set_input_enabled(false);
            editor.set_use_autoclose(false);
            editor.set_show_wrap_guides(false, cx);
            editor.set_show_indent_guides(false, cx);
            editor.set_use_modal_editing(false);
            editor.disable_mouse_wheel_zoom();
            editor.set_soft_wrap_mode(language::language_settings::SoftWrap::EditorWidth, cx);
            editor
        });

        let session = RunSession {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            icon,
            configuration_key: key,
            configuration_snapshot: configuration,
            command: command.clone(),
            args: args.clone(),
            cwd: cwd.clone(),
            env: env.clone(),
            state: SessionState::Starting,
            editor,
            scroll_to_end: true,
            soft_wrap: true,
            child_pid: None,
            execution_task: None,
            link_ranges: Vec::new(),
        };

        self.sessions.push(session);
        let idx = self.sessions.len() - 1;
        self.active_index = Some(idx);

        self.spawn_process(idx, command, args, cwd, env, window, cx);
        cx.notify();
    }

    fn spawn_process(
        &mut self,
        session_idx: usize,
        command_str: String,
        args: Vec<String>,
        cwd: PathBuf,
        env: BTreeMap<String, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session = match self.sessions.get_mut(session_idx) {
            Some(s) => s,
            None => return,
        };

        // Banner: > command args
        let banner = format!("> {} {}\n\n", command_str, args.join(" "));
        Self::append_raw_to_editor(&session.editor, &banner, Some(Color::Muted), cx);

        let _editor = session.editor.clone();
        let weak_self = cx.weak_entity();
        let session_id = session.id.clone();

        let task = cx.spawn_in(window, async move |run_panel, cx| {
            let spawn_result = async {
                let mut std_cmd = std::process::Command::new(&command_str);
                std_cmd.args(&args);
                std_cmd.current_dir(&cwd);
                for (k, v) in &env {
                    std_cmd.env(k, v);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt as _;
                    std_cmd.process_group(0);
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt as _;
                    std_cmd.creation_flags(0x08000000 | 0x00000200);
                }
                std_cmd.stdout(std::process::Stdio::piped());
                std_cmd.stderr(std::process::Stdio::piped());

                let mut cmd = smol::process::Command::from(std_cmd);
                let mut child = cmd.spawn().context("Failed to spawn command")?;
                let pid = child.id();

                run_panel.update(cx, |this, cx| {
                    if let Some(session) = this.sessions.iter_mut().find(|s| s.id == session_id) {
                        session.child_pid = Some(pid);
                        session.state = SessionState::Running;
                        cx.notify();
                    }
                })?;

                let stdout = child.stdout.take().context("Missing stdout")?;
                let stderr = child.stderr.take().context("Missing stderr")?;

                let (tx, mut rx) = futures::channel::mpsc::unbounded::<(String, bool)>();
                let tx_out = tx.clone();
                let tx_err = tx.clone();
                drop(tx);

                let read_out = cx.background_spawn(async move {
                    let mut reader = smol::io::BufReader::new(stdout).lines();
                    while let Some(line) = reader.next().await {
                        if let Ok(line) = line {
                            let _ = tx_out.unbounded_send((format!("{line}\n"), false));
                        }
                    }
                });

                let read_err = cx.background_spawn(async move {
                    let mut reader = smol::io::BufReader::new(stderr).lines();
                    while let Some(line) = reader.next().await {
                        if let Ok(line) = line {
                            let _ = tx_err.unbounded_send((format!("{line}\n"), true));
                        }
                    }
                });

                let weak_panel = weak_self.clone();
                let id = session_id.clone();
                let pump = async {
                    while let Some((line, is_err)) = rx.next().await {
                        let _ = cx.update(|_, cx| {
                            if let Some(panel) = weak_panel.upgrade() {
                                panel.update(cx, |this, cx| {
                                    if let Some(s) = this.sessions.iter_mut().find(|s| s.id == id) {
                                        Self::append_output_line(s, &line, is_err, cx);
                                    }
                                });
                            }
                        });
                    }
                };

                let wait_status = async {
                    let _ = futures::join!(read_out, read_err);
                    child.status().await
                };

                let (_, status) = futures::join!(pump, wait_status);
                let status = status?;
                anyhow::Ok(status)
            }
            .await;

            run_panel
                .update(cx, |this, cx| {
                    if let Some(session) = this.sessions.iter_mut().find(|s| s.id == session_id) {
                        session.child_pid = None;
                        match spawn_result {
                            Ok(status) => {
                                let code = status.code().unwrap_or(0);
                                if status.success() {
                                    session.state = SessionState::Finished(code);
                                    let msg = format!("\nProcess finished with exit code {code}\n");
                                    Self::append_raw_to_editor(
                                        &session.editor,
                                        &msg,
                                        Some(Color::Success),
                                        cx,
                                    );
                                } else {
                                    session.state = SessionState::Failed(code);
                                    let msg = format!("\nProcess finished with exit code {code}\n");
                                    Self::append_raw_to_editor(
                                        &session.editor,
                                        &msg,
                                        Some(Color::Error),
                                        cx,
                                    );
                                }
                            }
                            Err(err) => {
                                session.state = SessionState::Error;
                                let msg = format!("\nProcess failed to run: {err:#}\n");
                                Self::append_raw_to_editor(
                                    &session.editor,
                                    &msg,
                                    Some(Color::Error),
                                    cx,
                                );
                            }
                        }
                        cx.notify();
                    }
                })
                .log_err();
        });

        session.execution_task = Some(task);
    }

    fn append_output_line(
        session: &mut RunSession,
        line: &str,
        is_error: bool,
        cx: &mut App,
    ) {
        let parsed = terminal::parse_ansi_text(line.as_bytes());
        let text = parsed.text;
        let color = if is_error {
            Some(Color::Error)
        } else {
            None
        };
        Self::append_raw_to_editor(&session.editor, &text, color, cx);

        // Auto-scroll to end if enabled
        if session.scroll_to_end {
            session.editor.update(cx, |editor, cx| {
                editor.request_autoscroll(editor::scroll::Autoscroll::fit(), cx);
            });
        }
    }

    fn append_raw_to_editor(
        editor: &Entity<Editor>,
        text: &str,
        _color: Option<Color>,
        cx: &mut App,
    ) {
        editor.update(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap();
            let len = buffer.read(cx).len();
            buffer.update(cx, |buffer, cx| {
                buffer.edit([(len..len, text)], None, cx);
            });
        });
    }

    fn clear_editor(editor: &Entity<Editor>, cx: &mut App) {
        editor.update(cx, |editor, cx| {
            let buffer = editor.buffer().read(cx).as_singleton().unwrap();
            let len = buffer.read(cx).len();
            buffer.update(cx, |buffer, cx| {
                buffer.edit([(0..len, "")], None, cx);
            });
        });
    }

    pub fn rerun_session(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get_mut(index) {
            Self::terminate_session_process(session);
            let command = session.command.clone();
            let args = session.args.clone();
            let cwd = session.cwd.clone();
            let env = session.env.clone();
            session.state = SessionState::Starting;
            Self::clear_editor(&session.editor, cx);
            self.spawn_process(index, command, args, cwd, env, window, cx);
            cx.notify();
        }
    }

    pub fn stop_session(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get_mut(index) {
            Self::terminate_session_process(session);
            session.state = SessionState::Stopped;
            Self::append_raw_to_editor(
                &session.editor,
                "\nProcess stopped by user.\n",
                Some(Color::Error),
                cx,
            );
            cx.notify();
        }
    }

    pub fn close_session(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.sessions.len() {
            let mut session = self.sessions.remove(index);
            Self::terminate_session_process(&mut session);
            if self.sessions.is_empty() {
                self.active_index = None;
            } else if let Some(active) = self.active_index {
                if active >= self.sessions.len() {
                    self.active_index = Some(self.sessions.len() - 1);
                }
            }
            cx.notify();
        }
    }

    pub fn clear_output(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get(index) {
            Self::clear_editor(&session.editor, cx);
            cx.notify();
        }
    }

    pub fn toggle_scroll_to_end(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get_mut(index) {
            session.scroll_to_end = !session.scroll_to_end;
            cx.notify();
        }
    }

    pub fn toggle_soft_wrap(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(session) = self.sessions.get_mut(index) {
            session.soft_wrap = !session.soft_wrap;
            let wrap = if session.soft_wrap {
                language::language_settings::SoftWrap::EditorWidth
            } else {
                language::language_settings::SoftWrap::None
            };
            session.editor.update(cx, |editor, cx| {
                editor.set_soft_wrap_mode(wrap, cx);
            });
            cx.notify();
        }
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_idx = self.active_index;
        h_flex()
            .h(Tab::container_height(cx))
            .bg(cx.theme().colors().tab_bar_background)
            .border_b_1()
            .border_color(cx.theme().colors().border)
            .child(
                h_flex()
                    .px_2()
                    .gap_1()
                    .child(
                        Label::new("Run")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    
                    .gap_1()
                    .children(self.sessions.iter().enumerate().map(|(idx, session)| {
                        let selected = active_idx == Some(idx);
                        let _is_running = matches!(
                            session.state,
                            SessionState::Starting | SessionState::Running
                        );
                        let status_color = match session.state {
                            SessionState::Running | SessionState::Starting => Color::Success,
                            SessionState::Failed(_) | SessionState::Error => Color::Error,
                            SessionState::Finished(_) | SessionState::Stopped => Color::Muted,
                        };
                        h_flex()
                            .id(("run-tab", idx))
                            .px_2()
                            .py_0p5()
                            .gap_1p5()
                            .cursor_pointer()
                            .rounded_t_sm()
                            .when(selected, |this| {
                                this.bg(cx.theme().colors().tab_active_background)
                                    .border_b_2()
                                    .border_color(cx.theme().colors().border_focused)
                            })
                            .when(!selected, |this| {
                                this.bg(cx.theme().colors().tab_inactive_background)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.active_index = Some(idx);
                                cx.notify();
                            }))
                            .child(
                                Icon::new(session.icon)
                                    .size(IconSize::XSmall)
                                    .color(status_color),
                            )
                            .child(
                                Label::new(session.name.clone())
                                    .size(LabelSize::Small)
                                    .color(if selected { Color::Default } else { Color::Muted }),
                            )
                            .child(
                                IconButton::new(("close-tab", idx), IconName::Close)
                                    .icon_size(IconSize::XSmall)
                                    .tooltip(Tooltip::text("Close session"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.close_session(idx, cx);
                                    })),
                            )
                    })),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active_idx = self.active_index;
        let active_session = self.active_session();
        let is_running = active_session.is_some_and(|s| {
            matches!(s.state, SessionState::Starting | SessionState::Running)
        });
        let scroll_to_end = active_session.is_some_and(|s| s.scroll_to_end);
        let soft_wrap = active_session.is_some_and(|s| s.soft_wrap);

        v_flex()
            .w(px(32.))
            .h_full()
            .bg(cx.theme().colors().toolbar_background)
            .border_r_1()
            .border_color(cx.theme().colors().border)
            .py_1()
            .gap_1()
            .items_center()
            .child(
                IconButton::new("rerun-active-session", IconName::Rerun)
                    .icon_size(IconSize::Small)
                    .disabled(active_idx.is_none())
                    .tooltip(Tooltip::text("Rerun (Shift+F10)"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(idx) = this.active_index {
                            this.rerun_session(idx, window, cx);
                        }
                    })),
            )
            .child(
                IconButton::new("stop-active-session", IconName::Stop)
                    .icon_size(IconSize::Small)
                    .disabled(!is_running)
                    .icon_color(if is_running { Color::Error } else { Color::Muted })
                    .tooltip(Tooltip::text("Stop (Ctrl+F2)"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(idx) = this.active_index {
                            this.stop_session(idx, cx);
                        }
                    })),
            )
            .child(
                IconButton::new("scroll-to-end-session", IconName::ChevronDown)
                    .icon_size(IconSize::Small)
                    .disabled(active_idx.is_none())
                    .toggle_state(scroll_to_end)
                    .tooltip(Tooltip::text("Scroll to End"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(idx) = this.active_index {
                            this.toggle_scroll_to_end(idx, cx);
                        }
                    })),
            )
            .child(
                IconButton::new("clear-session-output", IconName::Trash)
                    .icon_size(IconSize::Small)
                    .disabled(active_idx.is_none())
                    .tooltip(Tooltip::text("Clear Output"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(idx) = this.active_index {
                            this.clear_output(idx, cx);
                        }
                    })),
            )
            .child(
                IconButton::new("soft-wrap-session", IconName::ArrowDown)
                    .icon_size(IconSize::Small)
                    .disabled(active_idx.is_none())
                    .toggle_state(soft_wrap)
                    .tooltip(Tooltip::text("Toggle Soft Wrap"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(idx) = this.active_index {
                            this.toggle_soft_wrap(idx, cx);
                        }
                    })),
            )
    }
}

impl Render for RunPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active_session = self.active_session();

        v_flex()
            .key_context("RunPanel")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().colors().surface_background)
            .child(self.render_tabs(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(self.render_toolbar(cx))
                    .child(
                        div()
                            .id("run-panel-output-area")
                            .flex_1()
                            .size_full()
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .when_some(active_session, |this, session| {
                                this.child(session.editor.clone())
                            })
                            .when(active_session.is_none(), |this| {
                                this.flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Label::new("No active run sessions")
                                            .color(Color::Muted)
                                            .size(LabelSize::Small),
                                    )
                            }),
                    ),
            )
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_state_transitions() {
        assert_eq!(SessionState::Starting, SessionState::Starting);
        assert_ne!(SessionState::Starting, SessionState::Running);
        assert_eq!(SessionState::Finished(0), SessionState::Finished(0));
        assert_ne!(SessionState::Finished(0), SessionState::Failed(1));
        assert_eq!(SessionState::Stopped, SessionState::Stopped);
    }

    #[test]
    fn test_ansi_parsing_in_run_panel() {
        let ansi_text = "\x1b[32mServer running on port 3000\x1b[0m\n";
        let parsed = terminal::parse_ansi_text(ansi_text.as_bytes());
        assert_eq!(parsed.text, "Server running on port 3000\n");
    }
}
