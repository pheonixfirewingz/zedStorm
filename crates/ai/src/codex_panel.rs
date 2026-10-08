use super::codex_protocol::{self, AccessMode, MessageKind, ServerRequest, Session};
use editor::Editor;
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, Subscription,
    Task, WeakEntity, Window, actions,
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use serde_json::{Value, json};
use settings::{Settings as _, SettingsStore};
use smol::channel;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};
use ui::{CircularProgress, ContextMenu, DropdownMenu, DropdownStyle, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    Panel, Workspace,
    dock::{DockPosition, PanelEvent},
};

actions!(codex_chat, [SendPrompt, Stop]);

struct QuestionInput {
    id: String,
    question: String,
    options: Vec<(String, String)>,
    editor: Entity<Editor>,
}

pub struct CodexPanel {
    backend: super::Backend,
    inactive_session: Option<Session>,
    mistral_conversation: super::mistral_api::SharedConversation,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    prompt: Entity<Editor>,
    session: Session,
    outgoing: Option<channel::Sender<Value>>,
    connection_task: Option<Task<()>>,
    event_task: Option<Task<()>>,
    markdown: HashMap<String, Entity<Markdown>>,
    scroll_handle: ScrollHandle,
    question_request_id: Option<Value>,
    questions: Vec<QuestionInput>,
    position: DockPosition,
    remote: bool,
    _subscriptions: Vec<Subscription>,
    last_markdown_sync: Instant,
    markdown_sync_task: Option<Task<()>>,
}

impl CodexPanel {
    pub fn new(workspace: &Workspace, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let prompt = cx.new(|cx| {
            let mut editor = Editor::auto_height(3, 10, window, cx);
            editor.set_placeholder_text("Ask anything…", window, cx);
            editor
        });
        let subscription = cx.observe(&prompt, |_, _, cx| cx.notify());
        let settings_subscription = cx.observe_global::<SettingsStore>(|panel, cx| {
            if !super::VibeSettings::get_global(cx).enabled
                && panel.backend == super::Backend::Mistral
            {
                panel.session.disconnected("Vibe is disabled".into());
                panel.switch_backend(super::Backend::Codex, cx);
            }
            cx.notify();
        });
        let credentials_subscription =
            cx.observe_global::<super::VibeCredentialsChanged>(|panel, cx| {
                if panel.backend == super::Backend::Mistral {
                    panel.connect(cx);
                }
            });
        Self {
            backend: super::Backend::Codex,
            inactive_session: None,
            mistral_conversation: Default::default(),
            workspace: workspace.weak_handle(),
            focus_handle: cx.focus_handle(),
            prompt,
            session: Session::new(project_directory(workspace, cx)),
            outgoing: None,
            connection_task: None,
            event_task: None,
            markdown: HashMap::new(),
            scroll_handle: ScrollHandle::new(),
            question_request_id: None,
            questions: Vec::new(),
            position: DockPosition::Right,
            remote: workspace.project().read(cx).is_remote(),
            _subscriptions: vec![
                subscription,
                settings_subscription,
                credentials_subscription,
            ],
            last_markdown_sync: Instant::now(),
            markdown_sync_task: None,
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.remote {
            self.session.error = Some(
                "Chat currently supports local projects. Open this project locally to use chat."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.connection_task.take();
        self.event_task.take();
        self.markdown_sync_task.take();
        if self.session.thread_id.is_none() {
            if let Some(workspace) = self.workspace.upgrade() {
                self.session.directory = project_directory(workspace.read(cx), cx);
            }
        }
        let (outgoing, outgoing_receiver) = channel::bounded(64);
        let (incoming_sender, incoming) = channel::bounded(128);
        self.outgoing = Some(outgoing);
        let initialize = self.session.initialize();
        self.transmit(initialize);
        let directory = self.session.directory.clone();
        let credentials = if self.backend == super::Backend::Mistral {
            Some(cx.read_credentials(super::VIBE_CREDENTIAL_KEY))
        } else {
            None
        };
        self.connection_task = Some(cx.background_spawn(codex_protocol::run_server(
            self.backend,
            cx.http_client(),
            self.mistral_conversation.clone(),
            directory,
            credentials,
            outgoing_receiver,
            incoming_sender,
        )));
        self.event_task = Some(cx.spawn(async move |panel, cx| {
            while let Ok(message) = incoming.recv().await {
                let mut batch = vec![message];
                while let Ok(next) = incoming.try_recv() {
                    batch.push(next);
                    if batch.len() >= 64 {
                        break;
                    }
                }

                let mut disconnected = false;
                let result = panel.update(cx, |panel, cx| {
                    for msg in batch {
                        match msg {
                            Ok(message) => {
                                let followups = panel.session.receive(message);
                                for followup in followups {
                                    panel.transmit(followup);
                                }
                                if let Some(url) = panel.session.auth_url.take() {
                                    cx.open_url(&url);
                                }
                            }
                            Err(error) => {
                                panel.session.disconnected(format!("{error:#}"));
                                panel.outgoing = None;
                                disconnected = true;
                            }
                        }
                    }
                    panel.schedule_sync_markdown(cx);
                    cx.notify();
                });
                if let Err(error) = result {
                    log::debug!("Codex panel closed: {error}");
                    break;
                }
                if disconnected {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn transmit(&mut self, message: Value) {
        let result = self
            .outgoing
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Chat is disconnected"))
            .and_then(|outgoing| outgoing.try_send(message).map_err(anyhow::Error::from));
        if let Err(error) = result {
            self.session.disconnected(format!(
                "Could not send to {}: {error}",
                self.backend.name()
            ));
            self.outgoing = None;
        }
    }

    fn schedule_sync_markdown(&mut self, cx: &mut Context<Self>) {
        const MIN_MARKDOWN_SYNC_INTERVAL: Duration = Duration::from_millis(50);
        let now = Instant::now();
        if now.duration_since(self.last_markdown_sync) >= MIN_MARKDOWN_SYNC_INTERVAL
            || !self.session.busy
        {
            self.last_markdown_sync = now;
            self.markdown_sync_task = None;
            self.sync_markdown(cx);
        } else if self.markdown_sync_task.is_none() {
            let delay = MIN_MARKDOWN_SYNC_INTERVAL
                .saturating_sub(now.duration_since(self.last_markdown_sync));
            self.markdown_sync_task = Some(cx.spawn(async move |panel, cx| {
                cx.background_executor().timer(delay).await;
                panel
                    .update(cx, |panel, cx| {
                        panel.last_markdown_sync = Instant::now();
                        panel.markdown_sync_task = None;
                        panel.sync_markdown(cx);
                        cx.notify();
                    })
                    .ok();
            }));
        }
    }

    fn sync_markdown(&mut self, cx: &mut Context<Self>) {
        let follow_output =
            self.scroll_handle.max_offset().y + self.scroll_handle.offset().y < px(48.);
        for message in &self.session.messages {
            if message.kind == MessageKind::Assistant {
                if let Some(markdown) = self.markdown.get(&message.id) {
                    if markdown.read(cx).source().as_ref() != message.text {
                        markdown.update(cx, |markdown, cx| {
                            markdown.reset(message.text.clone().into(), cx)
                        });
                    }
                } else {
                    let source = message.text.clone().into();
                    let markdown = cx.new(|cx| Markdown::new(source, None, None, cx));
                    self.markdown.insert(message.id.clone(), markdown);
                }
            }
        }
        if follow_output {
            self.scroll_handle.scroll_to_bottom();
        }
    }

    fn send(&mut self, _: &SendPrompt, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.prompt.read(cx).text(cx);
        if let Some(request) = self.session.send_prompt(text) {
            self.transmit(request);
            if self.session.busy {
                self.prompt
                    .update(cx, |editor, cx| editor.set_text("", window, cx));
            }
            self.scroll_handle.scroll_to_bottom();
            cx.notify();
        }
    }

    fn stop(&mut self, _: &Stop, _: &mut Window, cx: &mut Context<Self>) {
        if self.session.busy {
            if let Some(request) = self.session.interrupt() {
                self.transmit(request);
            }
            cx.notify();
        }
    }

    fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.busy {
            return;
        }
        self.connection_task.take();
        self.event_task.take();
        self.markdown_sync_task.take();
        self.outgoing = None;
        if self.backend == super::Backend::Mistral {
            self.mistral_conversation = Default::default();
        }
        let settings = self.session.settings.clone();
        self.session = Session::new(self.session.directory.clone());
        self.session.settings = settings;
        self.markdown.clear();
        self.questions.clear();
        self.question_request_id = None;
        self.connect(cx);
        self.prompt.focus_handle(cx).focus(window, cx);
    }

    fn render_settings(&self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let panel = cx.entity().downgrade();
        let model_menu = ContextMenu::build(window, cx, |menu, _, cx| {
            let default_panel = panel.clone();
            let mut menu = menu.toggleable_entry(
                format!("{} — configured model", self.backend.name()),
                self.session.settings.model.is_none(),
                IconPosition::Start,
                None,
                move |_, cx| {
                    default_panel
                        .update(cx, |panel, cx| {
                            if !panel.session.busy {
                                panel.session.settings.model = None;
                                panel.session.settings.reasoning_effort = None;
                                cx.notify();
                            }
                        })
                        .log_err();
                },
            );
            for model in &self.session.models {
                let panel = panel.clone();
                let model_id = model.model.clone();
                menu = menu.toggleable_entry(
                    model.display_name.clone(),
                    self.session.settings.model.as_ref() == Some(&model_id),
                    IconPosition::Start,
                    None,
                    move |_, cx| {
                        panel
                            .update(cx, |panel, cx| {
                                if !panel.session.busy {
                                    panel.session.settings.model = Some(model_id.clone());
                                    panel.session.settings.reasoning_effort = None;
                                    cx.notify();
                                }
                            })
                            .log_err();
                    },
                );
            }
            for backend in [super::Backend::Codex, super::Backend::Mistral] {
                if backend == self.backend
                    || (backend == super::Backend::Mistral
                        && !super::VibeSettings::get_global(cx).enabled)
                {
                    continue;
                }
                let panel = panel.clone();
                menu = menu.toggleable_entry(
                    backend.name(),
                    false,
                    IconPosition::Start,
                    None,
                    move |_, cx| {
                        panel
                            .update(cx, |panel, cx| panel.switch_backend(backend, cx))
                            .log_err();
                    },
                );
            }
            menu
        });
        let model = self
            .session
            .models
            .iter()
            .find(|model| Some(model.model.as_str()) == self.session.selected_model());
        let reasoning_menu = ContextMenu::build(window, cx, |menu, _, _| {
            let default_panel = panel.clone();
            let mut menu = menu.toggleable_entry(
                "Default reasoning",
                self.session.settings.reasoning_effort.is_none(),
                IconPosition::Start,
                None,
                move |_, cx| {
                    default_panel
                        .update(cx, |panel, cx| {
                            if !panel.session.busy {
                                panel.session.settings.reasoning_effort = None;
                                cx.notify();
                            }
                        })
                        .log_err();
                },
            );
            if let Some(model) = model {
                for effort in model.supported_reasoning_efforts.iter().filter(|effort| {
                    matches!(
                        effort.reasoning_effort.as_str(),
                        "none" | "minimal" | "low" | "medium" | "high"
                    )
                }) {
                    let panel = panel.clone();
                    let effort = effort.reasoning_effort.clone();
                    menu = menu.toggleable_entry(
                        reasoning_label(Some(&effort)),
                        self.session.settings.reasoning_effort.as_ref() == Some(&effort),
                        IconPosition::Start,
                        None,
                        move |_, cx| {
                            panel
                                .update(cx, |panel, cx| {
                                    if !panel.session.busy {
                                        panel.session.settings.reasoning_effort =
                                            Some(effort.clone());
                                        cx.notify();
                                    }
                                })
                                .log_err();
                        },
                    );
                }
            }
            menu
        });
        let access_menu = ContextMenu::build(window, cx, |mut menu, _, _| {
            for access in AccessMode::ALL {
                let panel = panel.clone();
                menu = menu.toggleable_entry(
                    access.label(),
                    self.session.settings.access == access,
                    IconPosition::Start,
                    None,
                    move |_, cx| {
                        panel
                            .update(cx, |panel, cx| {
                                if !panel.session.busy {
                                    panel.session.settings.access = access;
                                    cx.notify();
                                }
                            })
                            .log_err();
                    },
                );
            }
            menu
        });
        let model_label = model
            .map(|model| model.display_name.as_str())
            .or(self.session.selected_model())
            .unwrap_or("Model");
        let reasoning_label = reasoning_label(self.session.selected_reasoning_effort());
        let access_label = self.session.access_label();
        let trigger = |label: &str, icon: Option<IconName>| {
            h_flex()
                .gap_1()
                .min_w_0()
                .when_some(icon, |element, icon| {
                    element.child(Icon::new(icon).size(IconSize::Small).color(Color::Muted))
                })
                .child(
                    Label::new(label.to_owned())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .truncate(),
                )
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(IconSize::XSmall)
                        .color(Color::Muted),
                )
                .into_any_element()
        };
        h_flex()
            .min_w_0()
            .flex_1()
            .flex_wrap()
            .gap_1()
            .child(DropdownMenu::new_with_element("codex-model-selector", trigger(model_label, Some(IconName::AiOpenAi)), model_menu)
                .no_chevron()
                .style(DropdownStyle::Ghost)
                .trigger_size(ButtonSize::Compact)
                .tab_index(0)
                .aria_label("Model")
                .aria_value(model_label.to_owned())
                .disabled(self.session.busy)
                .trigger_tooltip(Tooltip::text(self.session.models_error.clone().unwrap_or_else(|| "Choose the model for your next message".into()))))
            .child(DropdownMenu::new_with_element("codex-reasoning-selector", trigger(reasoning_label, None), reasoning_menu)
                .no_chevron()
                .style(DropdownStyle::Ghost)
                .trigger_size(ButtonSize::Compact)
                .tab_index(0)
                .aria_label("Reasoning level")
                .aria_value(reasoning_label)
                .disabled(self.session.busy || model.is_none_or(|model| model.supported_reasoning_efforts.is_empty()))
                .trigger_tooltip(Tooltip::text("Choose the reasoning level for your next message")))
            .child(DropdownMenu::new_with_element("codex-access-selector", trigger(access_label, Some(IconName::Lock)), access_menu)
                .no_chevron()
                .style(DropdownStyle::Ghost)
                .trigger_size(ButtonSize::Compact)
                .tab_index(0)
                .aria_label("Access")
                .aria_value(access_label)
                .disabled(self.session.busy)
                .trigger_tooltip(Tooltip::text(if self.backend == super::Backend::Mistral { "Read-only: project reads. Default and Workspace write: approve modifying tools. Full access: automatically approve project tools. Mistral API requires network in every mode." } else { "Read-only: no edits or network. Workspace write: project edits, approval for elevated access. Full access: unrestricted files and network, no approvals." })))
    }

    fn switch_backend(&mut self, backend: super::Backend, cx: &mut Context<Self>) {
        if self.session.busy
            || self.backend == backend
            || (backend == super::Backend::Mistral && !super::VibeSettings::get_global(cx).enabled)
        {
            return;
        }
        self.connection_task.take();
        self.event_task.take();
        self.markdown_sync_task.take();
        self.outgoing = None;
        let session = self
            .inactive_session
            .take()
            .unwrap_or_else(|| Session::new(self.session.directory.clone()));
        self.inactive_session = Some(std::mem::replace(&mut self.session, session));
        self.backend = backend;
        self.markdown.clear();
        self.questions.clear();
        self.question_request_id = None;
        self.connect(cx);
    }

    fn login(&mut self, cx: &mut Context<Self>) {
        if let Some(request) = self.session.login() {
            self.transmit(request);
        }
        cx.notify();
    }

    fn answer(&mut self, id: &Value, result: Value, cx: &mut Context<Self>) {
        if let Some(response) = self.session.answer(id, result) {
            self.transmit(response);
        }
        self.questions.clear();
        self.question_request_id = None;
        cx.notify();
    }

    fn sync_questions(
        &mut self,
        request: &ServerRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.question_request_id.as_ref() == Some(&request.id) {
            return;
        }
        self.questions.clear();
        self.question_request_id = Some(request.id.clone());
        if let Some(questions) = request.params.get("questions").and_then(Value::as_array) {
            for question in questions {
                let Some(id) = question.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let editor = cx.new(|cx| {
                    let mut editor = Editor::auto_height(1, 4, window, cx);
                    editor.set_placeholder_text("Your answer", window, cx);
                    if question.get("isSecret").and_then(Value::as_bool) == Some(true) {
                        editor.set_masked(true, cx);
                    }
                    editor
                });
                self.questions.push(QuestionInput {
                    id: id.into(),
                    question: question
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    options: question
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|options| {
                            options
                                .iter()
                                .filter_map(|option| {
                                    Some((
                                        option.get("label")?.as_str()?.to_owned(),
                                        option
                                            .get("description")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default()
                                            .to_owned(),
                                    ))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    editor,
                });
            }
        }
    }

    fn render_request(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let request = self.session.requests.first()?.clone();
        let id = request.id.clone();
        if request.method.ends_with("requestUserInput") {
            self.sync_questions(&request, window, cx);
            let inputs = self.questions.iter().enumerate().map(|(index, question)| {
                let options = question.options.iter().enumerate().map(
                    |(option_index, (label, description))| {
                        let editor = question.editor.clone();
                        let label = label.clone();
                        let answer = label.clone();
                        Button::new(("codex-answer-option", index * 100 + option_index), label)
                            .tooltip(Tooltip::text(description.clone()))
                            .on_click(cx.listener(move |_, _, window, cx| {
                                editor.update(cx, |editor, cx| {
                                    editor.set_text(answer.clone(), window, cx)
                                });
                            }))
                    },
                );
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(question.question.clone()))
                    .child(h_flex().flex_wrap().gap_1().children(options))
                    .child(
                        div()
                            .p_2()
                            .border_1()
                            .border_color(cx.theme().colors().border)
                            .rounded_md()
                            .child(question.editor.clone()),
                    )
            });
            return Some(
                v_flex()
                    .gap_3()
                    .p_3()
                    .border_t_1()
                    .border_color(cx.theme().colors().border)
                    .child(Label::new(format!(
                        "{} needs your input",
                        self.backend.name()
                    )))
                    .children(inputs)
                    .child(
                        Button::new("codex-submit-answers", "Submit Answers").on_click(
                            cx.listener(move |panel, _, _, cx| {
                                let answers = panel
                                    .questions
                                    .iter()
                                    .map(|question| {
                                        (
                                            question.id.clone(),
                                            json!({"answers": [question.editor.read(cx).text(cx)]}),
                                        )
                                    })
                                    .collect::<serde_json::Map<_, _>>();
                                panel.answer(&id, json!({"answers": answers}), cx);
                            }),
                        ),
                    )
                    .into_any_element(),
            );
        }
        let (title, details) = approval_description(&request);
        let allow_result = if request.method == "item/permissions/requestApproval" {
            json!({"permissions": request.params.get("permissions").cloned().unwrap_or(json!({})), "scope": "turn"})
        } else {
            json!({"decision": "accept"})
        };
        let decline_result = if request.method == "item/permissions/requestApproval" {
            json!({"permissions": {}, "scope": "turn"})
        } else {
            json!({"decision": "decline"})
        };
        let allow_id = id.clone();
        Some(
            v_flex()
                .gap_2()
                .p_3()
                .border_t_1()
                .border_color(cx.theme().colors().border)
                .child(Label::new(title))
                .child(
                    div()
                        .id("codex-approval-details")
                        .max_h(px(180.))
                        .overflow_y_scroll()
                        .text_sm()
                        .child(details),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("codex-allow", "Allow Once").on_click(cx.listener(
                                move |panel, _, _, cx| {
                                    panel.answer(&allow_id, allow_result.clone(), cx)
                                },
                            )),
                        )
                        .child(
                            Button::new("codex-decline", "Decline").on_click(cx.listener(
                                move |panel, _, _, cx| {
                                    panel.answer(&id, decline_result.clone(), cx)
                                },
                            )),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn reasoning_label(effort: Option<&str>) -> &'static str {
    match effort {
        Some("none") => "None",
        Some("minimal") => "Minimal",
        Some("low") => "Low",
        Some("medium") => "Medium",
        Some("high") => "High",
        _ => "Default",
    }
}

fn project_directory(workspace: &Workspace, cx: &App) -> PathBuf {
    workspace
        .worktrees(cx)
        .next()
        .and_then(|worktree| {
            let worktree = worktree.read(cx);
            let path = worktree.abs_path();
            if worktree.root_entry().is_some_and(|entry| entry.is_dir()) {
                Some(path.to_path_buf())
            } else {
                path.parent().map(|parent| parent.to_path_buf())
            }
        })
        .unwrap_or_else(|| paths::home_dir().to_path_buf())
}

fn approval_description(request: &ServerRequest) -> (&'static str, String) {
    let reason = request
        .params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match request.method.as_str() {
        "item/commandExecution/requestApproval" => {
            if let Some(host) = request
                .params
                .pointer("/networkApprovalContext/host")
                .and_then(Value::as_str)
            {
                ("Allow network access?", format!("{host}\n{reason}"))
            } else {
                let command = request
                    .params
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("Run command");
                let cwd = request
                    .params
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                ("Allow this command?", format!("{command}\n{cwd}\n{reason}"))
            }
        }
        "item/fileChange/requestApproval" => {
            let root = request
                .params
                .get("grantRoot")
                .and_then(Value::as_str)
                .unwrap_or_default();
            ("Allow file changes?", format!("{reason}\n{root}"))
        }
        _ => {
            let permissions = request
                .params
                .get("permissions")
                .cloned()
                .unwrap_or(Value::Null);
            (
                "Allow requested access?",
                format!("{reason}\n{permissions:#}"),
            )
        }
    }
}

impl Render for CodexPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let request = self.render_request(window, cx);
        let settings = self.render_settings(window, cx);
        let status = if self.remote {
            "Local projects only"
        } else if self.session.signing_in {
            "Finish sign-in in your browser"
        } else if !self.session.ready {
            if self.outgoing.is_some() {
                "Connecting…"
            } else {
                "Disconnected"
            }
        } else if self.session.needs_sign_in {
            "Sign in to Codex to start chatting"
        } else if !self.session.requests.is_empty() {
            "Waiting for your response"
        } else if self.session.busy {
            match self.backend {
                super::Backend::Codex => "Codex is working…",
                super::Backend::Mistral => "Mistral is working…",
            }
        } else if self.session.stopped {
            "Stopped"
        } else {
            "Ready"
        };
        let status_icon = if self.remote || self.session.needs_sign_in {
            IconName::Warning
        } else if self.session.signing_in || !self.session.requests.is_empty() {
            IconName::CircleHelp
        } else if !self.session.ready {
            if self.outgoing.is_some() {
                IconName::LoadCircle
            } else {
                IconName::Disconnected
            }
        } else if self.session.busy {
            IconName::LoadCircle
        } else if self.session.stopped {
            IconName::Stop
        } else {
            IconName::Check
        };
        let directory = self.session.directory.display().to_string();
        let can_send = self.session.ready
            && !self.session.busy
            && !self.session.needs_sign_in
            && !self.prompt.read(cx).text(cx).trim().is_empty();
        let messages = self
            .session
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let label = match message.kind {
                    MessageKind::User => "You",
                    MessageKind::Assistant => self.backend.name(),
                    MessageKind::Activity => "Activity",
                };
                let body = if let Some(markdown) = self.markdown.get(&message.id) {
                    div()
                        .child(MarkdownElement::new(
                            markdown.clone(),
                            MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                        ))
                        .into_any_element()
                } else {
                    let text: SharedString = message.text.clone().into();
                    div()
                        .id(("codex-message-body", index))
                        .when(message.kind == MessageKind::Activity, |element| {
                            element.max_h(px(200.)).overflow_y_scroll()
                        })
                        .text_sm()
                        .child(text)
                        .into_any_element()
                };
                v_flex()
                    .gap_2()
                    .p_3()
                    .min_w_0()
                    .rounded_md()
                    .when(message.kind == MessageKind::User, |element| {
                        element.bg(cx.theme().colors().element_background)
                    })
                    .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
                    .child(body)
            });
        v_flex()
            .id("codex-chat-panel")
            .size_full()
            .min_w_0()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::stop))
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(Label::new(self.backend.name()))
                            .child(
                                div()
                                    .id("codex-status")
                                    .child(
                                        Icon::new(status_icon)
                                            .size(IconSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .tooltip(Tooltip::text(format!("{status} · {directory}"))),
                            )
                            .when_some(self.session.model.clone(), |element, model| {
                                element.child(
                                    div()
                                        .id("codex-model")
                                        .min_w_0()
                                        .overflow_hidden()
                                        .child(
                                            Label::new(model.clone())
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .truncate(),
                                        )
                                        .tooltip(Tooltip::text(model)),
                                )
                            }),
                    )
                    .child(
                        IconButton::new("codex-new-chat", IconName::Plus)
                            .disabled(self.session.busy)
                            .tooltip(Tooltip::text("New chat"))
                            .on_click(
                                cx.listener(|panel, _, window, cx| panel.new_chat(window, cx)),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_4()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .children(
                        [
                            (
                                "codex-five-hour-usage",
                                "5-hour",
                                self.session.five_hour_usage,
                            ),
                            ("codex-weekly-usage", "Weekly", self.session.weekly_usage),
                        ]
                        .into_iter()
                        .map(|(id, label, usage)| {
                            let remaining = usage.map(|percent| 100.0 - percent);
                            let percentage = remaining
                                .map(|percent| format!("{percent:.0}%"))
                                .unwrap_or_else(|| "—".into());
                            let tooltip = remaining
                                .map(|percent| format!("{label} limit: {percent:.0}% left"))
                                .unwrap_or_else(|| {
                                    self.session
                                        .usage_error
                                        .clone()
                                        .unwrap_or_else(|| format!("{label} usage is unavailable"))
                                });
                            let color = match remaining {
                                Some(percent) if percent <= 10.0 => cx.theme().status().error,
                                Some(percent) if percent <= 25.0 => cx.theme().status().warning,
                                Some(_) => cx.theme().status().info,
                                None => cx.theme().colors().border_variant,
                            };
                            h_flex()
                                .id(id)
                                .gap_1p5()
                                .child(
                                    CircularProgress::new(
                                        remaining.unwrap_or(0.0),
                                        100.0,
                                        px(16.0),
                                        cx,
                                    )
                                    .stroke_width(px(2.0))
                                    .progress_color(color),
                                )
                                .child(
                                    Label::new(format!("{label} {percentage} left"))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .tooltip(Tooltip::text(tooltip))
                        }),
                    ),
            )
            .when(self.remote || self.session.signing_in, |element| {
                element.child(
                    div().px_3().py_2().child(
                        Label::new(status)
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .child(
                v_flex()
                    .id("codex-transcript")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .p_2()
                    .gap_3()
                    .children(messages),
            )
            .when_some(self.session.error.clone(), |element, error| {
                element.child(
                    v_flex()
                        .p_3()
                        .gap_2()
                        .border_t_1()
                        .border_color(cx.theme().colors().border)
                        .child(
                            div()
                                .id("codex-error")
                                .max_h(px(140.))
                                .overflow_y_scroll()
                                .text_sm()
                                .text_color(cx.theme().status().error)
                                .child(error),
                        )
                        .when(!self.session.ready && !self.remote, |element| {
                            element.child(
                                Button::new("codex-reconnect", "Reconnect")
                                    .on_click(cx.listener(|panel, _, _, cx| panel.connect(cx))),
                            )
                        }),
                )
            })
            .when(self.session.needs_sign_in, |element| {
                element.child(
                    div().px_3().pb_2().child(
                        Button::new("codex-sign-in", "Sign in")
                            .disabled(self.session.signing_in)
                            .on_click(cx.listener(|panel, _, _, cx| panel.login(cx))),
                    ),
                )
            })
            .children(request)
            .child(
                div().p_3().child(
                    v_flex()
                        .id("codex-composer")
                        .key_context("CodexChat")
                        .min_w_0()
                        .gap_4()
                        .px_3()
                        .pt_3()
                        .pb_2()
                        .rounded(px(18.))
                        .border_1()
                        .border_color(cx.theme().colors().border_variant)
                        .bg(cx.theme().colors().panel_background)
                        .child(div().min_w_0().child(self.prompt.clone()))
                        .child(
                            h_flex()
                                .min_w_0()
                                .justify_between()
                                .gap_1()
                                .child(settings)
                                .child(
                                    div()
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .size(px(32.))
                                        .rounded_full()
                                        .overflow_hidden()
                                        .bg(if self.session.busy {
                                            cx.theme().status().error
                                        } else if can_send {
                                            cx.theme().colors().text
                                        } else {
                                            cx.theme().colors().element_background
                                        })
                                        .child(if self.session.busy {
                                            IconButton::new("codex-stop", IconName::Stop)
                                                .aria_label("Stop")
                                                .shape(ui::IconButtonShape::Square)
                                                .size(ButtonSize::Large)
                                                .style(ButtonStyle::Transparent)
                                                .icon_size(IconSize::Small)
                                                .tooltip(Tooltip::text("Stop"))
                                                .on_click(cx.listener(|panel, _, window, cx| {
                                                    panel.stop(&Stop, window, cx)
                                                }))
                                        } else {
                                            IconButton::new("codex-send", IconName::ArrowUp)
                                                .aria_label("Send message")
                                                .shape(ui::IconButtonShape::Square)
                                                .size(ButtonSize::Large)
                                                .style(ButtonStyle::Transparent)
                                                .icon_size(IconSize::Small)
                                                .icon_color(if can_send {
                                                    Color::Custom(
                                                        cx.theme().colors().panel_background,
                                                    )
                                                } else {
                                                    Color::Muted
                                                })
                                                .disabled(!can_send)
                                                .tooltip(Tooltip::text(
                                                    "Send · Enter (Shift+Enter for a new line)",
                                                ))
                                                .on_click(cx.listener(|panel, _, window, cx| {
                                                    panel.send(&SendPrompt, window, cx)
                                                }))
                                        }),
                                ),
                        ),
                ),
            )
    }
}

impl Focusable for CodexPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for CodexPanel {}

impl Panel for CodexPanel {
    fn persistent_name() -> &'static str {
        "Codex Chat"
    }
    fn panel_key() -> &'static str {
        "codex_chat"
    }
    fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        self.prompt.focus_handle(cx)
    }
    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }
    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }
    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        self.position = position;
        cx.notify();
    }
    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(420.)
    }
    fn min_size(&self, _: &Window, _: &App) -> Option<Pixels> {
        Some(px(280.))
    }
    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        None
    }
    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Codex Chat")
    }
    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(zed_actions::OpenCodex)
    }
    fn activation_priority(&self) -> u32 {
        10
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active && self.outgoing.is_none() && self.connection_task.is_none() {
            // Dock activation happens inside a workspace update; read its project afterwards.
            let panel = cx.entity().downgrade();
            cx.defer(move |cx| {
                panel
                    .update(cx, |panel, cx| {
                        if panel.outgoing.is_none() && panel.connection_task.is_none() {
                            panel.connect(cx);
                        }
                    })
                    .log_err();
            });
        }
    }
}
