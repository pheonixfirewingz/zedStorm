use futures::StreamExt as _;
use std::{
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context as _, Result};
use collections::BTreeMap;
use editor::{Editor, EditorMode, HighlightKey, MultiBufferOffset, SizingBehavior};
use futures::io::AsyncBufReadExt;
use gpui::{
    App, AsyncWindowContext, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    IntoElement, ParentElement, Pixels, Render, Styled, Subscription, Task, UnderlineStyle,
    WeakEntity, Window, actions, div, px,
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

fn captured_command(command: std::process::Command) -> smol::process::Command {
    let mut command = smol::process::Command::from(command);
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command
}

fn run_command(
    configuration: &RunConfiguration,
    command: &str,
    arguments: &[String],
) -> Result<std::process::Command> {
    let shell = configuration
        .run
        .as_ref()
        .map(|run| run.shell.clone())
        .unwrap_or_default();
    let builder = task::ShellBuilder::new(&shell, cfg!(windows)).non_interactive();
    let kind = builder.kind();
    let mut command_line = if configuration.npm.is_some() || std::path::Path::new(command).is_file()
    {
        kind.try_quote_prefix_aware(command)
            .context("Could not quote the executable")?
            .into_owned()
    } else {
        command.to_owned()
    };
    for argument in arguments {
        command_line.push(' ');
        command_line.push_str(
            &kind
                .try_quote(argument)
                .context("Could not quote a run argument")?,
        );
    }
    let mut process = builder.build_std_command(None, &[]);
    let arguments = kind.args_for_shell(false, command_line);
    #[cfg(windows)]
    if kind == util::shell::ShellKind::Cmd {
        use std::os::windows::process::CommandExt as _;
        for argument in arguments {
            process.raw_arg(argument);
        }
    } else {
        process.args(arguments);
    }
    #[cfg(not(windows))]
    process.args(arguments);
    Ok(process)
}

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
            if session.child_pid.is_some() {
                task.detach();
            }
        }
        if let Some(pid) = session.child_pid.take() {
            #[cfg(unix)]
            {
                std::process::Command::new("kill")
                    .args(["-TERM", "--", &format!("-{pid}")])
                    .output()
                    .log_err();
            }
            #[cfg(windows)]
            {
                util::command::new_std_command("taskkill")
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .output()
                    .log_err();
            }
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

        // A detached process from a previous run must not update the replacement session.
        session.id = uuid::Uuid::new_v4().to_string();
        let configuration = session.configuration_snapshot.clone();
        let weak_self = cx.weak_entity();
        let session_id = session.id.clone();

        let task = cx.spawn_in(window, async move |run_panel, cx| {
            let spawn_result = async {
                let mut std_cmd = run_command(&configuration, &command_str, &args)?;
                if !env.contains_key("NO_COLOR")
                    && !env.contains_key("FORCE_COLOR")
                    && std::env::var_os("NO_COLOR").is_none()
                    && std::env::var_os("FORCE_COLOR").is_none()
                {
                    std_cmd.env("FORCE_COLOR", "1");
                }
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
                let mut cmd = captured_command(std_cmd);
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
                        let line = line.context("Could not read process stdout")?;
                        if tx_out.unbounded_send((format!("{line}\n"), false)).is_err() {
                            break;
                        }
                    }
                    anyhow::Ok(())
                });

                let read_err = cx.background_spawn(async move {
                    let mut reader = smol::io::BufReader::new(stderr).lines();
                    while let Some(line) = reader.next().await {
                        let line = line.context("Could not read process stderr")?;
                        if tx_err.unbounded_send((format!("{line}\n"), true)).is_err() {
                            break;
                        }
                    }
                    anyhow::Ok(())
                });

                let weak_panel = weak_self.clone();
                let id = session_id.clone();
                let pump = async {
                    while let Some((line, is_err)) = rx.next().await {
                        cx.update(|_, cx| {
                            if let Some(panel) = weak_panel.upgrade() {
                                panel.update(cx, |this, cx| {
                                    if let Some(s) = this.sessions.iter_mut().find(|s| s.id == id) {
                                        Self::append_output_line(s, &line, is_err, cx);
                                    }
                                });
                            }
                        })
                        .log_err();
                    }
                };

                let wait_status = async {
                    let (stdout, stderr) = futures::join!(read_out, read_err);
                    stdout?;
                    stderr?;
                    anyhow::Ok(child.status().await?)
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
                        if session.state == SessionState::Stopped {
                            return;
                        }
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
        let color = if is_error { Some(Color::Error) } else { None };
        Self::append_styled_to_editor(&session.editor, parsed, color, cx);

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
        color: Option<Color>,
        cx: &mut App,
    ) {
        Self::append_styled_to_editor(
            editor,
            terminal::parse_ansi_text(text.as_bytes()),
            color,
            cx,
        );
    }

    fn append_styled_to_editor(
        editor: &Entity<Editor>,
        parsed: terminal::ParsedAnsiText,
        fallback_color: Option<Color>,
        cx: &mut App,
    ) {
        editor.update(cx, |editor, cx| {
            let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
                return;
            };
            let offset = buffer.read(cx).len();
            buffer.update(cx, |buffer, cx| {
                buffer.edit([(offset..offset, parsed.text.as_str())], None, cx);
            });
            let snapshot = editor.buffer().read(cx).snapshot(cx);
            for (range, color) in parsed.foreground_spans {
                let color = color
                    .map(|color| terminal_view::terminal_element::convert_color(&color, cx.theme()))
                    .or_else(|| fallback_color.map(|color| color.color(cx)));
                let Some(color) = color else { continue };
                let start = offset + range.start;
                let range = snapshot.anchor_after(MultiBufferOffset(start))
                    ..snapshot.anchor_before(MultiBufferOffset(offset + range.end));
                editor.highlight_text_key(
                    HighlightKey::ConsoleAnsiHighlight(start),
                    vec![range],
                    HighlightStyle {
                        color: Some(color),
                        ..Default::default()
                    },
                    false,
                    cx,
                );
            }
            for (range, color) in parsed.background_spans {
                let Some(color) = color else { continue };
                let start = offset + range.start;
                let range = snapshot.anchor_after(MultiBufferOffset(start))
                    ..snapshot.anchor_before(MultiBufferOffset(offset + range.end));
                editor.highlight_background(
                    HighlightKey::ConsoleAnsiHighlight(start),
                    &[range],
                    move |_, theme| {
                        if terminal::is_default_background_color(color) {
                            theme.colors().terminal_background
                        } else {
                            terminal_view::terminal_element::convert_color(&color, theme)
                        }
                    },
                    cx,
                );
            }
            let mut finder = linkify::LinkFinder::new();
            finder.kinds(&[linkify::LinkKind::Url]);
            let links = finder
                .links(&parsed.text)
                .map(|link| {
                    snapshot.anchor_after(MultiBufferOffset(offset + link.start()))
                        ..snapshot.anchor_before(MultiBufferOffset(offset + link.end()))
                })
                .collect::<Vec<_>>();
            if !links.is_empty() {
                editor.highlight_text_key(
                    HighlightKey::Editor,
                    links,
                    HighlightStyle {
                        color: Some(Color::Accent.color(cx)),
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        }),
                        ..Default::default()
                    },
                    true,
                    cx,
                );
            }
        });
    }

    fn clear_editor(editor: &Entity<Editor>, cx: &mut App) {
        editor.update(cx, |editor, cx| {
            editor.clear_highlights_with(
                &mut |key| {
                    matches!(
                        key,
                        HighlightKey::ConsoleAnsiHighlight(_) | HighlightKey::Editor
                    )
                },
                cx,
            );
            editor.clear_background_highlights_with(
                |key| matches!(key, HighlightKey::ConsoleAnsiHighlight(_)),
                cx,
            );
            let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
                return;
            };
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

    #[gpui::test]
    async fn runner_preserves_colours_and_opens_links(cx: &mut gpui::TestAppContext) {
        crate::tests::init_test(cx);
        let window = cx.add_window(|window, cx| {
            let editor = Editor::multi_line(window, cx);
            window.focus(&editor.focus_handle(cx), cx);
            editor
        });
        let mut context =
            editor::test::editor_test_context::EditorTestContext::for_editor(window, cx).await;
        let editor = context.editor.clone();
        context.update(|_, cx| {
            editor.update(cx, |editor, _| {
                editor.set_read_only(true);
                editor.set_input_enabled(false);
            });
            RunPanel::append_raw_to_editor(&editor, "Starting\n", Some(Color::Muted), cx);
            RunPanel::append_raw_to_editor(
                &editor,
                "\x1b[32mready\x1b[0m \x1b[44mbanner\x1b[0m http://localhost:3000/path?q=1\n",
                None,
                cx,
            );
        });
        context.update_editor(|editor, window, cx| {
            let (style, ranges) = editor
                .text_highlights(HighlightKey::ConsoleAnsiHighlight(9), cx)
                .expect("green output highlight");
            assert_eq!(
                style.color,
                Some(terminal_view::terminal_element::convert_color(
                    &terminal::Color::Named(terminal::NamedColor::Green),
                    cx.theme(),
                ))
            );
            assert_eq!(ranges.len(), 1);
            let (style, ranges) = editor
                .text_highlights(HighlightKey::Editor, cx)
                .expect("URL highlight");
            assert!(style.underline.is_some());
            assert_eq!(ranges.len(), 1);
            assert_eq!(editor.all_text_background_highlights(window, cx).len(), 1);
        });
        let position =
            context.pixel_position("Starting\nready banner http://localˇhost:3000/path?q=1\n");
        context.simulate_mouse_move(position, None, gpui::Modifiers::secondary_key());
        context.run_until_parked();
        context.simulate_click(position, gpui::Modifiers::secondary_key());
        context.run_until_parked();
        assert_eq!(
            context.opened_url().as_deref(),
            Some("http://localhost:3000/path?q=1")
        );
        context.update(|_, cx| RunPanel::clear_editor(&editor, cx));
        context.update_editor(|editor, window, cx| {
            assert!(editor.all_text_highlights(window, cx).is_empty());
            assert!(editor.all_text_background_highlights(window, cx).is_empty());
        });
    }

    #[test]
    fn run_process_exposes_both_output_streams() -> Result<()> {
        smol::block_on(async {
            let mut configuration =
                crate::run_configurations::registry::ConfigurationProvider::create_default(
                    "node",
                    std::path::Path::new("."),
                    None,
                );
            let run = configuration.run.as_mut().context("Missing Run target")?;
            run.shell =
                task::Shell::Program(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into());
            let command = run_command(
                &configuration,
                "echo runner-output && echo runner-error >&2",
                &[],
            )?;
            let mut child = captured_command(command).spawn()?;
            let mut stdout = child.stdout.take().context("Missing stdout")?;
            let mut stderr = child.stderr.take().context("Missing stderr")?;
            let mut output = String::new();
            let mut error = String::new();
            use futures::io::AsyncReadExt as _;
            stdout.read_to_string(&mut output).await?;
            stderr.read_to_string(&mut error).await?;
            assert!(child.status().await?.success());
            assert!(output.contains("runner-output"));
            assert!(error.contains("runner-error"));
            anyhow::Ok(())
        })
    }

    #[test]
    fn run_shell_commands_and_preserve_arguments() -> Result<()> {
        let mut configuration =
            crate::run_configurations::registry::ConfigurationProvider::create_default(
                "node",
                std::path::Path::new("."),
                None,
            );
        let run = configuration.run.as_mut().context("Missing Run target")?;
        run.shell = task::Shell::Program(if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.into());
        let output = run_command(&configuration, "echo first && echo second", &[])?.output()?;
        ensure_success(&output)?;
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("first"));
        assert!(text.contains("second"));

        let output = run_command(&configuration, "echo", &["two words".into()])?.output()?;
        ensure_success(&output)?;
        let text = String::from_utf8(output.stdout)?;
        assert!(text.contains("two words"));
        assert!(!text.contains("'two words'"));
        Ok(())
    }

    fn ensure_success(output: &std::process::Output) -> Result<()> {
        anyhow::ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

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
