use super::codex_protocol::{self, AccessMode, MessageKind, ServerRequest, Session};
use editor::Editor;
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, PromptLevel, ScrollHandle,
    Subscription, Task, WeakEntity, Window, actions,
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use serde_json::{Value, json};
use settings::{Settings as _, SettingsStore};
use smol::channel;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use ui::{CircularProgress, ContextMenu, DropdownMenu, DropdownStyle, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    Panel, Workspace,
    dock::{DockPosition, PanelEvent},
};

actions!(codex_chat, [SendPrompt, Stop, DeleteChat]);

struct QuestionInput {
    id: String,
    question: String,
    options: Vec<(String, String)>,
    editor: Entity<Editor>,
}

struct ChatHistoryEntry {
    identity: Rc<()>,
    title: String,
    backend: super::Backend,
    session: Session,
    mistral_conversation: super::mistral_api::SharedConversation,
}

pub struct CodexPanel {
    chat_identity: Rc<()>,
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
    expanded_activity_groups: HashSet<String>,
    scroll_handle: ScrollHandle,
    question_request_id: Option<Value>,
    questions: Vec<QuestionInput>,
    position: DockPosition,
    remote: bool,
    _subscriptions: Vec<Subscription>,
    last_markdown_sync: Instant,
    markdown_sync_task: Option<Task<()>>,
    _usage_refresh_task: Task<()>,
    chat_history: Vec<ChatHistoryEntry>,
    history_sender: Option<channel::Sender<super::chat_store::SavedChats>>,
    _history_error_task: Option<Task<()>>,
    history_writer_task: Option<Task<()>>,
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
        let usage_refresh_task = cx.spawn(async move |panel, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(60))
                    .await;
                if let Err(error) = panel.update(cx, |panel, cx| {
                    if panel.backend == super::Backend::Codex
                        && (panel.session.five_hour_resets_at.is_some()
                            || panel.session.weekly_resets_at.is_some())
                    {
                        cx.notify();
                    }
                }) {
                    log::debug!("Codex panel closed: {error}");
                    break;
                }
            }
        });
        let mut panel = Self {
            chat_identity: Rc::new(()),
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
            expanded_activity_groups: HashSet::new(),
            scroll_handle: ScrollHandle::new(),
            question_request_id: None,
            questions: Vec::new(),
            position: DockPosition::Right,
            remote: workspace.project().read(cx).is_remote(),
            _subscriptions: vec![
                subscription,
                settings_subscription,
                credentials_subscription,
                cx.on_app_quit(|panel, _| {
                    panel.save_history();
                    panel.history_sender.take();
                    let writer = panel.history_writer_task.take();
                    async move {
                        if let Some(writer) = writer {
                            writer.await;
                        }
                    }
                }),
                cx.on_release(|panel, _| {
                    panel.save_history();
                    panel.history_sender.take();
                    if let Some(writer) = panel.history_writer_task.take() {
                        writer.detach();
                    }
                }),
            ],
            last_markdown_sync: Instant::now(),
            markdown_sync_task: None,
            _usage_refresh_task: usage_refresh_task,
            chat_history: Vec::new(),
            history_sender: None,
            _history_error_task: None,
            history_writer_task: None,
        };
        panel.restore_history(cx);
        panel.sync_markdown(cx);
        panel
    }

    fn restore_history(&mut self, cx: &mut Context<Self>) {
        if self.remote {
            return;
        }
        let path = super::chat_store::storage_path(&self.session.directory);
        match super::chat_store::load(&path) {
            Ok(Some(chats)) => {
                let (backend, session, conversation) = chats.active.into_session();
                self.backend = backend;
                self.session = session;
                if backend == super::Backend::Mistral {
                    self.mistral_conversation = conversation;
                }
                if let Some(inactive) = chats.inactive {
                    let (backend, session, conversation) = inactive.into_session();
                    self.inactive_session = Some(session);
                    if backend == super::Backend::Mistral {
                        self.mistral_conversation = conversation;
                    }
                }
                for chat in chats.history {
                    let (backend, session, mistral_conversation) = chat.into_session();
                    self.chat_history.push(ChatHistoryEntry {
                        identity: Rc::new(()),
                        title: chat_title(&session),
                        backend,
                        session,
                        mistral_conversation,
                    });
                }
                if self.backend == super::Backend::Mistral
                    && !super::VibeSettings::get_global(cx).enabled
                {
                    let session = self
                        .inactive_session
                        .take()
                        .unwrap_or_else(|| Session::new(self.session.directory.clone()));
                    self.inactive_session = Some(std::mem::replace(&mut self.session, session));
                    self.backend = super::Backend::Codex;
                }
            }
            Ok(None) => {}
            Err(error) => {
                log::error!("Could not restore chat history: {error:#}");
                self.session.error = Some(format!("Could not restore chat history: {error:#}"));
                return;
            }
        }
        let (sender, receiver) = channel::unbounded();
        let (errors, error_receiver) = channel::unbounded();
        self.history_writer_task = Some(cx.background_spawn(async move {
            while let Ok(mut chats) = receiver.recv().await {
                while let Ok(newer) = receiver.try_recv() {
                    chats = newer;
                }
                if let Err(error) = super::chat_store::save(&path, &chats) {
                    log::error!("Could not save chat history: {error:#}");
                    if errors
                        .send(format!("Could not save chat history: {error:#}"))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }));
        self.history_sender = Some(sender);
        self._history_error_task = Some(cx.spawn(async move |panel, cx| {
            while let Ok(error) = error_receiver.recv().await {
                if let Err(error) = panel.update(cx, |panel, cx| {
                    panel.session.error = Some(error);
                    cx.notify();
                }) {
                    log::debug!("Chat panel closed: {error}");
                    break;
                }
            }
        }));
    }

    fn save_history(&mut self) {
        let Some(sender) = &self.history_sender else {
            return;
        };
        let result = (|| -> anyhow::Result<()> {
            let capture =
                |backend,
                 session: &Session,
                 conversation: &super::mistral_api::SharedConversation| {
                    let conversation = if backend == super::Backend::Mistral {
                        conversation
                            .lock()
                            .map_err(|_| anyhow::anyhow!("Mistral conversation lock failed"))?
                            .clone()
                    } else {
                        Default::default()
                    };
                    Ok::<_, anyhow::Error>(super::chat_store::SavedChat::capture(
                        backend,
                        session,
                        conversation,
                    ))
                };
            let inactive_backend = match self.backend {
                super::Backend::Codex => super::Backend::Mistral,
                super::Backend::Mistral => super::Backend::Codex,
            };
            let chats = super::chat_store::SavedChats {
                active: capture(self.backend, &self.session, &self.mistral_conversation)?,
                inactive: self
                    .inactive_session
                    .as_ref()
                    .map(|session| capture(inactive_backend, session, &self.mistral_conversation))
                    .transpose()?,
                history: self
                    .chat_history
                    .iter()
                    .map(|entry| {
                        capture(entry.backend, &entry.session, &entry.mistral_conversation)
                    })
                    .collect::<anyhow::Result<_>>()?,
            };
            sender.try_send(chats)?;
            Ok(())
        })();
        if let Err(error) = result {
            log::error!("Could not save chat history: {error:#}");
            self.session.error = Some(format!("Could not save chat history: {error:#}"));
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
                    panel.save_history();
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
            self.save_history();
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
        self.archive_current_chat();
        self.chat_identity = Rc::new(());
        if self.backend == super::Backend::Mistral {
            self.mistral_conversation = Default::default();
        }
        let settings = self.session.settings.clone();
        self.session = Session::new(self.session.directory.clone());
        self.session.settings = settings;
        self.markdown.clear();
        self.expanded_activity_groups.clear();
        self.questions.clear();
        self.question_request_id = None;
        self.connect(cx);
        self.prompt.focus_handle(cx).focus(window, cx);
        self.save_history();
    }

    fn archive_current_chat(&mut self) {
        if !self
            .session
            .messages
            .iter()
            .any(|message| message.kind == MessageKind::User)
        {
            return;
        }
        let title = chat_title(&self.session);
        let mut session = Session::new(self.session.directory.clone());
        session.settings = self.session.settings.clone();
        let session = std::mem::replace(&mut self.session, session);
        self.chat_history.insert(
            0,
            ChatHistoryEntry {
                identity: self.chat_identity.clone(),
                title,
                backend: self.backend,
                session,
                mistral_conversation: self.mistral_conversation.clone(),
            },
        );
    }

    fn switch_to_history(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.busy {
            return;
        }
        let Some(entry) = self.chat_history.get(index) else {
            return;
        };
        if entry.backend != self.backend {
            return;
        }
        let entry = self.chat_history.remove(index);
        self.archive_current_chat();
        self.connection_task.take();
        self.event_task.take();
        self.markdown_sync_task.take();
        self.outgoing = None;
        self.session = entry.session;
        self.chat_identity = entry.identity;
        self.mistral_conversation = entry.mistral_conversation;
        self.markdown.clear();
        self.expanded_activity_groups.clear();
        self.questions.clear();
        self.question_request_id = None;
        self.connect(cx);
        self.sync_markdown(cx);
        self.scroll_handle.scroll_to_bottom();
        self.prompt.focus_handle(cx).focus(window, cx);
        self.save_history();
    }

    fn confirm_delete_chat(
        &mut self,
        identity: Rc<()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.session.busy {
            return;
        }
        let title = if Rc::ptr_eq(&identity, &self.chat_identity) {
            chat_title(&self.session)
        } else if let Some(entry) = self
            .chat_history
            .iter()
            .find(|entry| Rc::ptr_eq(&entry.identity, &identity))
        {
            entry.title.clone()
        } else {
            return;
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete chat \"{title}\"?"),
            Some("This removes the chat from this project's saved history and cannot be undone."),
            &["Cancel", "Delete"],
            cx,
        );
        cx.spawn_in(window, async move |panel, cx| {
            if answer.await != Ok(1) {
                return;
            }
            panel
                .update_in(cx, |panel, window, cx| {
                    if panel.session.busy {
                        return;
                    }
                    if Rc::ptr_eq(&identity, &panel.chat_identity) {
                        panel.connection_task.take();
                        panel.event_task.take();
                        panel.markdown_sync_task.take();
                        panel.outgoing = None;
                        let settings = panel.session.settings.clone();
                        panel.session = Session::new(panel.session.directory.clone());
                        panel.session.settings = settings;
                        panel.chat_identity = Rc::new(());
                        if panel.backend == super::Backend::Mistral {
                            panel.mistral_conversation = Default::default();
                        }
                        panel.markdown.clear();
                        panel.expanded_activity_groups.clear();
                        panel.questions.clear();
                        panel.question_request_id = None;
                        panel.connect(cx);
                        panel.prompt.focus_handle(cx).focus(window, cx);
                    } else if let Some(index) = panel
                        .chat_history
                        .iter()
                        .position(|entry| Rc::ptr_eq(&entry.identity, &identity))
                    {
                        panel.chat_history.remove(index);
                    } else {
                        return;
                    }
                    panel.save_history();
                    cx.notify();
                })
                .log_err();
        })
        .detach();
    }

    fn render_history(&self, window: &mut Window, cx: &mut Context<Self>) -> DropdownMenu {
        let panel = cx.entity().downgrade();
        let menu = ContextMenu::build(window, cx, |mut menu, _, _| {
            menu = menu
                .header("Recent chats")
                .end_slot_action(Box::new(DeleteChat));
            let mut has_history = false;
            for (index, entry) in self.chat_history.iter().enumerate() {
                if entry.backend != self.backend {
                    continue;
                }
                has_history = true;
                let panel = panel.clone();
                let delete_panel = panel.clone();
                let identity = entry.identity.clone();
                menu = menu.entry_with_end_slot(
                    entry.title.clone(),
                    None,
                    move |window, cx| {
                        panel
                            .update(cx, |panel, cx| panel.switch_to_history(index, window, cx))
                            .log_err();
                    },
                    IconName::Trash,
                    "Delete chat".into(),
                    move |window, cx| {
                        delete_panel
                            .update(cx, |panel, cx| {
                                panel.confirm_delete_chat(identity.clone(), window, cx)
                            })
                            .log_err();
                    },
                );
            }
            if !has_history {
                menu = menu.header("No previous chats for this project");
            }
            menu
        });
        DropdownMenu::new("codex-chat-history", "History", menu)
            .style(DropdownStyle::Ghost)
            .trigger_size(ButtonSize::Compact)
            .tab_index(0)
            .aria_label("Chat history")
            .disabled(self.session.busy)
            .trigger_tooltip(Tooltip::text("Reopen a recent chat"))
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
        self.chat_identity = Rc::new(());
        self.markdown.clear();
        self.expanded_activity_groups.clear();
        self.questions.clear();
        self.question_request_id = None;
        self.connect(cx);
        self.sync_markdown(cx);
        self.save_history();
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

fn chat_title(session: &Session) -> String {
    let title = session
        .messages
        .iter()
        .find(|message| message.kind == MessageKind::User)
        .and_then(|message| message.text.lines().next())
        .unwrap_or("Untitled chat")
        .chars()
        .take(48)
        .collect::<String>();
    if title.is_empty() {
        "Untitled chat".into()
    } else {
        title
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let request = self.render_request(window, cx);
        let settings = self.render_settings(window, cx);
        let history = self.render_history(window, cx);
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
            .chunk_by(|first, second| {
                first.kind == MessageKind::Activity && second.kind == MessageKind::Activity
            })
            .enumerate()
            .filter_map(|(index, messages)| {
                let message = messages.first()?;
                if message.kind == MessageKind::Activity {
                    let group_id = message.id.clone();
                    let expanded = self.expanded_activity_groups.contains(&group_id);
                    let summary = messages
                        .last()
                        .and_then(|message| message.text.lines().next())
                        .unwrap_or("Activity");
                    let label = if messages.len() == 1 {
                        "1 activity".to_owned()
                    } else {
                        format!("{} activities", messages.len())
                    };
                    return Some(
                        v_flex()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        Button::new(("codex-activity-toggle", index), label)
                                            .style(ButtonStyle::Transparent)
                                            .size(ButtonSize::Compact)
                                            .label_size(LabelSize::Small)
                                            .color(Color::Muted)
                                            .start_icon(
                                                Icon::new(if expanded {
                                                    IconName::ChevronDown
                                                } else {
                                                    IconName::ChevronRight
                                                })
                                                .size(IconSize::Small)
                                                .color(Color::Muted),
                                            )
                                            .aria_expanded(expanded)
                                            .on_click(cx.listener(move |panel, _, _, cx| {
                                                if !panel.expanded_activity_groups.remove(&group_id)
                                                {
                                                    panel
                                                        .expanded_activity_groups
                                                        .insert(group_id.clone());
                                                }
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id(("codex-activity-summary", index))
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .child(
                                                Label::new(summary.to_owned())
                                                    .size(LabelSize::Small)
                                                    .color(Color::Muted)
                                                    .truncate(),
                                            )
                                            .tooltip(Tooltip::text(summary.to_owned())),
                                    ),
                            )
                            .when(expanded, |element| {
                                element.child(
                                    v_flex()
                                        .id(("codex-activity-details", index))
                                        .min_w_0()
                                        .max_h(px(320.))
                                        .overflow_y_scroll()
                                        .ml_2()
                                        .pl_2()
                                        .border_l_1()
                                        .border_color(cx.theme().colors().border_variant)
                                        .gap_2()
                                        .children(messages.iter().enumerate().map(
                                            |(activity_index, message)| {
                                                div()
                                                    .id(("codex-activity-output", activity_index))
                                                    .min_w_0()
                                                    .p_2()
                                                    .rounded_md()
                                                    .bg(cx.theme().colors().element_background)
                                                    .text_sm()
                                                    .text_color(cx.theme().colors().text_muted)
                                                    .child(message.text.clone())
                                            },
                                        )),
                                )
                            })
                            .into_any_element(),
                    );
                }
                let body = if let Some(markdown) = self.markdown.get(&message.id) {
                    div()
                        .min_w_0()
                        .child(MarkdownElement::new(
                            markdown.clone(),
                            MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                        ))
                        .into_any_element()
                } else {
                    div()
                        .id(("codex-message-body", index))
                        .min_w_0()
                        .text_sm()
                        .child(message.text.clone())
                        .into_any_element()
                };
                Some(
                    v_flex()
                        .min_w_0()
                        .px_1()
                        .py_1()
                        .when(message.kind == MessageKind::User, |element| {
                            element
                                .self_end()
                                .max_w(gpui::relative(0.9))
                                .my_1()
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .bg(cx.theme().colors().element_background)
                        })
                        .child(body)
                        .into_any_element(),
                )
            })
            .collect::<Vec<_>>();

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
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
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
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .child(history)
                            .child(
                                IconButton::new("codex-delete-chat", IconName::Trash)
                                    .aria_label("Delete current chat")
                                    .disabled(self.session.busy || self.session.messages.is_empty())
                                    .tooltip(Tooltip::text("Delete current chat"))
                                    .on_click(cx.listener(|panel, _, window, cx| {
                                        panel.confirm_delete_chat(
                                            panel.chat_identity.clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                IconButton::new("codex-new-chat", IconName::Plus)
                                    .aria_label("New chat")
                                    .disabled(self.session.busy)
                                    .tooltip(Tooltip::text("New chat"))
                                    .on_click(cx.listener(|panel, _, window, cx| {
                                        panel.new_chat(window, cx)
                                    })),
                            ),
                    ),
            )
            .when(self.backend == super::Backend::Codex, |element| {
                element.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
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
                                    self.session.five_hour_resets_at,
                                ),
                                (
                                    "codex-weekly-usage",
                                    "Weekly",
                                    self.session.weekly_usage,
                                    self.session.weekly_resets_at,
                                ),
                            ]
                            .into_iter()
                            .map(|(id, label, usage, resets_at)| {
                                let reset = reset_countdown(resets_at, now);
                                let remaining = usage.map(|percent| 100.0 - percent);
                                let percentage = remaining
                                    .map(|percent| format!("{percent:.0}%"))
                                    .unwrap_or_else(|| "—".into());
                                let mut tooltip = remaining
                                    .map(|percent| format!("{label} limit: {percent:.0}% left"))
                                    .unwrap_or_else(|| {
                                        self.session.usage_error.clone().unwrap_or_else(|| {
                                            format!("{label} usage is unavailable")
                                        })
                                    });
                                tooltip.push_str(&format!(" · {reset}"));
                                let color = match remaining {
                                    Some(percent) if percent <= 10.0 => cx.theme().status().error,
                                    Some(percent) if percent <= 25.0 => cx.theme().status().warning,
                                    Some(_) => cx.theme().status().info,
                                    None => cx.theme().colors().border_variant,
                                };
                                v_flex()
                                    .id(id)
                                    .flex_1()
                                    .min_w(px(150.0))
                                    .gap_1()
                                    .child(
                                        h_flex()
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
                                                Label::new(format!(
                                                    "{label} {percentage} left - {reset}"
                                                ))
                                                .size(LabelSize::Small)
                                                .color(Color::Muted)
                                                .truncate(),
                                            ),
                                    )
                                    .tooltip(Tooltip::text(tooltip))
                            }),
                        ),
                )
            })
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
                    .px_3()
                    .py_2()
                    .gap_2()
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
                        .gap_2()
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

fn reset_countdown(resets_at: Option<u64>, now: u64) -> String {
    let Some(resets_at) = resets_at else {
        return "Reset unavailable".into();
    };
    let seconds = resets_at.saturating_sub(now);
    if seconds == 0 {
        return "Reset due".into();
    }
    let minutes = seconds.div_ceil(60);
    let days = minutes / (24 * 60);
    let hours = minutes / 60 % 24;
    let minutes = minutes % 60;
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::reset_countdown;

    #[test]
    fn reset_countdowns_handle_boundaries_and_missing_timestamps() {
        assert_eq!(reset_countdown(None, 100), "Reset unavailable");
        assert_eq!(reset_countdown(Some(99), 100), "Reset due");
        assert_eq!(reset_countdown(Some(100), 100), "Reset due");
        assert_eq!(reset_countdown(Some(101), 100), "1m");
        assert_eq!(reset_countdown(Some(100 + 3599), 100), "1h 0m");
        assert_eq!(reset_countdown(Some(100 + 5 * 3600), 100), "5h 0m");
        assert_eq!(
            reset_countdown(Some(100 + 3 * 86400 + 2 * 3600 + 60), 100),
            "3d 2h 1m"
        );
    }
}
