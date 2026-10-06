pub mod running;

use crate::{
    debugger_panel::DebugPanel, persistence::SerializedLayout, session::running::DebugTerminal,
};
use dap::client::SessionId;
use gpui::{App, Axis, Entity, EventEmitter, FocusHandle, Focusable, Task, WeakEntity};
use project::debugger::session::Session;

use project::{Project, debugger::session::SessionQuirks};
use rpc::proto;
use running::RunningState;
use ui::prelude::*;
use util::ResultExt;
use workspace::{
    CollaboratorId, FollowableItem, Pane, ViewId, Workspace,
    item::{self, Item},
};

pub struct DebugSession {
    pub(crate) panel: Option<WeakEntity<DebugPanel>>,
    pub(crate) host_pane: Option<WeakEntity<Pane>>,
    remote_id: Option<workspace::ViewId>,
    pub(crate) running_state: Entity<RunningState>,
    pub(crate) quirks: SessionQuirks,
}

impl DebugSession {
    pub(crate) fn running(
        project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        parent_terminal: Option<Entity<DebugTerminal>>,
        session: Entity<Session>,
        serialized_layout: Option<SerializedLayout>,
        dock_axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let running_state = cx.new(|cx| {
            RunningState::new(
                session.clone(),
                project.clone(),
                workspace.clone(),
                parent_terminal,
                serialized_layout,
                dock_axis,
                window,
                cx,
            )
        });
        let quirks = session.read(cx).quirks();

        cx.new(|cx| {
            cx.observe(&running_state, |_, _, cx| {
                cx.emit(());
                cx.notify();
            })
            .detach();
            Self {
                panel: None,
                host_pane: None,
                remote_id: None,
                running_state,
                quirks,
            }
        })
    }

    pub(crate) fn session_id(&self, cx: &App) -> SessionId {
        self.running_state.read(cx).session_id()
    }

    pub fn session(&self, cx: &App) -> Entity<Session> {
        self.running_state.read(cx).session().clone()
    }

    pub(crate) fn shutdown(&mut self, cx: &mut Context<Self>) {
        self.running_state
            .update(cx, |state, cx| state.shutdown(cx));
    }

    pub(crate) fn label(&self, cx: &mut App) -> Option<SharedString> {
        let session = self.running_state.read(cx).session().clone();
        session.update(cx, |session, cx| {
            let session_label = session.label();
            let quirks = session.quirks();
            let mut single_thread_name = || {
                let threads = session.threads(cx);
                match threads.as_slice() {
                    [(thread, _)] => Some(SharedString::from(&thread.name)),
                    _ => None,
                }
            };
            if quirks.prefer_thread_name {
                single_thread_name().or(session_label)
            } else {
                session_label.or_else(single_thread_name)
            }
        })
    }

    pub fn running_state(&self) -> &Entity<RunningState> {
        &self.running_state
    }
}

impl EventEmitter<()> for DebugSession {}

impl Focusable for DebugSession {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.running_state.focus_handle(cx)
    }
}

impl Item for DebugSession {
    type Event = ();

    fn can_split(&self) -> bool {
        false
    }

    fn to_item_events(_: &(), emit: &mut dyn FnMut(item::ItemEvent)) {
        emit(item::ItemEvent::UpdateTab);
    }
    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.session(cx)
            .read(cx)
            .label()
            .unwrap_or_else(|| "Debug Session".into())
    }

    fn tab_content(&self, params: item::TabContentParams, _: &Window, cx: &App) -> AnyElement {
        let session = self.session(cx);
        let (icon, color, status) = if session.read(cx).is_terminated() {
            (IconName::Power, Color::Muted, "Finished")
        } else if session.read(cx).is_building() {
            (IconName::Clock, Color::Muted, "Building")
        } else if self.running_state.read(cx).thread_status(cx)
            == Some(project::debugger::session::ThreadStatus::Stopped)
        {
            (IconName::DebugPause, Color::Warning, "Paused")
        } else {
            (IconName::Debug, Color::Success, "Running")
        };
        h_flex()
            .gap_1()
            .child(Icon::new(icon).size(IconSize::Small).color(color))
            .child(
                Label::new(self.tab_content_text(0, cx))
                    .size(LabelSize::Small)
                    .color(params.text_color()),
            )
            .child(
                Label::new(status)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .into_any_element()
    }

    fn handle_drop(
        &self,
        active_pane: &Pane,
        dropped: &dyn std::any::Any,
        window: &mut Window,
        cx: &mut App,
    ) -> bool {
        let Some(tab) = dropped.downcast_ref::<workspace::pane::DraggedTab>() else {
            return true;
        };
        if tab.item.downcast::<DebugSession>().is_none() {
            return true;
        }
        let Some(direction) = active_pane.drag_split_direction() else {
            return false;
        };
        let Some(target) = self.host_pane.as_ref().and_then(WeakEntity::upgrade) else {
            return true;
        };
        let Some(panel) = self.panel.clone() else {
            return true;
        };
        let source = tab.pane.clone();
        let item_id = tab.item.item_id();
        // The source or target pane may still be updating during the drop.
        window.defer(cx, move |window, cx| {
            panel
                .update(cx, |panel, cx| {
                    panel.split_session_pane(&source, &target, item_id, direction, window, cx);
                })
                .log_err();
        });
        true
    }
}

impl FollowableItem for DebugSession {
    fn remote_id(&self) -> Option<workspace::ViewId> {
        self.remote_id
    }

    fn to_state_proto(&self, _window: &mut Window, _cx: &mut App) -> Option<proto::view::Variant> {
        None
    }

    fn from_state_proto(
        _workspace: Entity<Workspace>,
        _remote_id: ViewId,
        _state: &mut Option<proto::view::Variant>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<gpui::Task<anyhow::Result<Entity<Self>>>> {
        None
    }

    fn add_event_to_update_proto(
        &self,
        _event: &Self::Event,
        _update: &mut Option<proto::update_view::Variant>,
        _window: &mut Window,
        _cx: &mut App,
    ) -> bool {
        // update.get_or_insert_with(|| proto::update_view::Variant::DebugPanel(Default::default()));

        true
    }

    fn apply_update_proto(
        &mut self,
        _project: &Entity<project::Project>,
        _message: proto::update_view::Variant,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> gpui::Task<anyhow::Result<()>> {
        Task::ready(Ok(()))
    }

    fn set_leader_id(
        &mut self,
        _leader_id: Option<CollaboratorId>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn to_follow_event(_event: &Self::Event) -> Option<workspace::item::FollowEvent> {
        None
    }

    fn dedup(&self, existing: &Self, _window: &Window, cx: &App) -> Option<workspace::item::Dedup> {
        if existing.session_id(cx) == self.session_id(cx) {
            Some(item::Dedup::KeepExisting)
        } else {
            None
        }
    }

    fn is_project_item(&self, _window: &Window, _cx: &App) -> bool {
        true
    }
}

impl Render for DebugSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let controls = DebugPanel::session_controls(&self.running_state, window, cx);
        let content = self
            .running_state
            .update(cx, |this, cx| this.render(window, cx).into_any_element());
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .w_full()
                    .p_1()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(controls),
            )
            .child(div().flex_1().min_h_0().size_full().child(content))
    }
}
