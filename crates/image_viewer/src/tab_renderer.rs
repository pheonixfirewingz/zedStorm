use anyhow::{Context as _, Result, anyhow};
use async_channel::{Receiver, Sender};
use base64::{Engine, engine::general_purpose::STANDARD};
use extension::ExtensionTabRendererBackend;
use futures::{
    AsyncBufReadExt, AsyncWriteExt, FutureExt, StreamExt, channel::oneshot, io::BufReader,
};
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, Modifiers, MouseButton, ParentElement, Pixels, Point, Render, RenderImage,
    ScrollDelta, SharedString, Styled, Task, Window, canvas, div,
};
use project::{Project, ProjectEntryId, ProjectPath};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Stdio, sync::Arc};
use ui::prelude::*;
use util::ResultExt;
use workspace::{
    Pane,
    item::{Item, ItemBufferKind, ItemEvent, ProjectItem, SaveOptions},
};

pub(super) struct TabRendererFile {
    project: Entity<Project>,
    project_path: ProjectPath,
    path: PathBuf,
    dirty: bool,
}

impl project::ProjectItem for TabRendererFile {
    fn try_open(
        project: &Entity<Project>,
        path: &ProjectPath,
        cx: &mut App,
    ) -> Option<Task<Result<Entity<Self>>>> {
        let absolute_path = project.read(cx).absolute_path(path, cx)?;
        if !project.read(cx).is_local() {
            return None;
        }

        let is_supported =
            extension::TabRendererRegistry::renderer_for_path(&absolute_path, cx).is_some();

        if !is_supported {
            return None;
        }

        let file = cx.new(|_| Self {
            project: project.clone(),
            project_path: path.clone(),
            path: absolute_path,
            dirty: false,
        });
        Some(Task::ready(Ok(file)))
    }

    fn entry_id(&self, cx: &App) -> Option<ProjectEntryId> {
        self.project
            .read(cx)
            .entry_for_path(&self.project_path, cx)
            .map(|entry| entry.id)
    }

    fn project_path(&self, _: &App) -> Option<ProjectPath> {
        Some(self.project_path.clone())
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }
}

enum BackendEvent {
    Frame(Arc<RenderImage>),
    Modified(bool),
    Saved(bool),
    SaveError(String),
    Error(String),
}

pub(super) struct TabRendererView {
    file: Entity<TabRendererFile>,
    focus: FocusHandle,
    frame: Option<Arc<RenderImage>>,
    bounds: Option<Bounds<Pixels>>,
    commands: Sender<Value>,
    error: Option<SharedString>,
    active: bool,
    last_motion: Option<Value>,
    save_waiters: Vec<oneshot::Sender<Result<(), String>>>,
    _process: Task<()>,
    _events: Task<()>,
}

impl TabRendererView {
    fn new(file: Entity<TabRendererFile>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (commands, input) = async_channel::bounded(128);
        let (output, events) = async_channel::bounded(2);
        let path = file.read(cx).path.clone();

        let backend_query =
            extension::TabRendererRegistry::renderer_for_path(&path, cx);

        let Some((_, _, callback)) = backend_query else {
            return Self {
                file,
                focus: cx.focus_handle(),
                frame: None,
                bounds: None,
                commands,
                error: Some("No tab renderer registered for this file type".into()),
                active: false,
                last_motion: None,
                save_waiters: Vec::new(),
                _process: Task::ready(()),
                _events: Task::ready(()),
            };
        };

        let backend_future = callback(path.clone(), cx);
        let process = cx.background_spawn(async move {
            let backend_res = backend_future.await;

            match backend_res {
                Ok(ExtensionTabRendererBackend::Command(cmd)) => {
                    let binary = cmd.path;
                    let args = cmd.args;
                    let env = cmd.env;
                    if let Err(error) = run_command_backend(binary, args, env, input, output.clone()).await {
                        let _ = output.send(BackendEvent::Error(format!("{error:#}"))).await;
                    }
                }
                Ok(ExtensionTabRendererBackend::WebEngine(_)) => {
                    let _ = output
                        .send(BackendEvent::Error(
                            "Extensions using a web engine must provide an executable command backend".into(),
                        ))
                        .await;
                }
                Err(error) => {
                    let _ = output.send(BackendEvent::Error(format!("{error:#}"))).await;
                }
            }
        });

        let event_task = cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = events.recv().await {
                if view
                    .update_in(cx, |view, window, cx| {
                        match event {
                            BackendEvent::Frame(frame) => {
                                if let Some(previous) = view.frame.replace(frame) {
                                    window.drop_image(previous).log_err();
                                }
                            }
                            BackendEvent::Modified(modified) => {
                                view.file.update(cx, |file, _| file.dirty = modified);
                                cx.emit(ItemEvent::UpdateTab);
                            }
                            BackendEvent::Saved(modified) => {
                                view.file.update(cx, |file, _| file.dirty = modified);
                                view.error = None;
                                view.finish_save(if modified {
                                    Err("The document changed while saving. Save again to include latest edits.".into())
                                } else {
                                    Ok(())
                                });
                                cx.emit(ItemEvent::UpdateTab);
                            }
                            BackendEvent::SaveError(error) | BackendEvent::Error(error) => {
                                view.finish_save(Err(error.clone()));
                                view.error = Some(error.into());
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        cx.on_release_in(window, |view, window, _| {
            if let Some(frame) = view.frame.take() {
                window.drop_image(frame).log_err();
            }
        })
        .detach();

        Self {
            file,
            focus: cx.focus_handle(),
            frame: None,
            bounds: None,
            commands,
            error: None,
            active: false,
            last_motion: None,
            save_waiters: Vec::new(),
            _process: process,
            _events: event_task,
        }
    }

    fn finish_save(&mut self, result: Result<(), String>) {
        for waiter in self.save_waiters.drain(..) {
            let _ = waiter.send(result.clone());
        }
    }

    fn send(&mut self, message: Value, cx: &mut Context<Self>) {
        match self.commands.try_send(message) {
            Ok(()) => {}
            Err(async_channel::TrySendError::Full(message)) if message["kind"] == "move" => {
                self.last_motion = None;
            }
            Err(error) => {
                self.error =
                    Some(format!("Could not communicate with tab renderer backend: {error}").into());
                cx.notify();
            }
        }
    }

    fn pointer(
        &mut self,
        position: Point<Pixels>,
        button: MouseButton,
        pressed: bool,
        clicks: usize,
        modifiers: Modifiers,
        moving: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(bounds) = self.bounds {
            let position = self.surface_position(position, bounds);
            let message = json!({
                "action": "pointer",
                "kind": if moving { "move" } else { "button" },
                "x": position.x,
                "y": position.y,
                "button": button_number(button),
                "pressed": pressed,
                "clicks": clicks,
                "modifiers": modifier_mask(modifiers) | if moving && pressed { 1 << (7 + button_number(button)) } else { 0 }
            });
            if moving {
                if self.last_motion.as_ref() == Some(&message) {
                    return;
                }
                self.last_motion = Some(message.clone());
            } else {
                self.last_motion = None;
            }
            self.send(message, cx);
        }
    }

    fn surface_position(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> Point<f32> {
        let position = position - bounds.origin;
        let width = f32::from(bounds.size.width).max(1.0);
        let height = f32::from(bounds.size.height).max(1.0);
        let surface = self.frame.as_ref().map(|frame| frame.size(0));
        gpui::point(
            f32::from(position.x) * surface.map_or(width, |size| size.width.0 as f32) / width,
            f32::from(position.y) * surface.map_or(height, |size| size.height.0 as f32) / height,
        )
    }
}

impl Focusable for TabRendererView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<ItemEvent> for TabRendererView {}

impl Item for TabRendererView {
    type Event = ItemEvent;

    fn tab_content_text(&self, _: usize, cx: &App) -> SharedString {
        self.file
            .read(cx)
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Tab".into())
            .into()
    }

    fn tab_tooltip_text(&self, cx: &App) -> Option<SharedString> {
        Some(
            self.file
                .read(cx)
                .path
                .to_string_lossy()
                .into_owned()
                .into(),
        )
    }

    fn to_item_events(event: &Self::Event, callback: &mut dyn FnMut(ItemEvent)) {
        callback(*event);
    }

    fn for_each_project_item(
        &self,
        cx: &App,
        callback: &mut dyn FnMut(gpui::EntityId, &dyn project::ProjectItem),
    ) {
        callback(self.file.entity_id(), self.file.read(cx));
    }

    fn buffer_kind(&self, _: &App) -> ItemBufferKind {
        ItemBufferKind::Singleton
    }

    fn is_dirty(&self, cx: &App) -> bool {
        self.file.read(cx).dirty
    }

    fn can_save(&self, _: &App) -> bool {
        true
    }

    fn save(
        &mut self,
        _: SaveOptions,
        _: Entity<Project>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        if !self.file.read(cx).dirty {
            return Task::ready(Ok(()));
        }
        let (sender, receiver) = oneshot::channel();
        match self.commands.try_send(json!({ "action": "save" })) {
            Ok(()) => self.save_waiters.push(sender),
            Err(error) => return Task::ready(Err(anyhow!("Could not save tab content: {error}"))),
        }
        cx.background_spawn(async move {
            receiver
                .await
                .context("Tab renderer editor closed before saving")?
                .map_err(anyhow::Error::msg)
        })
    }

    fn reload(
        &mut self,
        _: Entity<Project>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        self.send(json!({ "action": "reload" }), cx);
        Task::ready(Ok(()))
    }

    fn deactivated(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.active = false;
        self.send(json!({ "action": "active", "active": false }), cx);
    }

    fn show_toolbar(&self) -> bool {
        false
    }
}

impl ProjectItem for TabRendererView {
    type Item = TabRendererFile;

    fn for_project_item(
        _: Entity<Project>,
        _: Option<&Pane>,
        file: Entity<TabRendererFile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(file, window, cx)
    }
}

impl Render for TabRendererView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let frame = self.frame.clone();
        div()
            .id("tab-renderer")
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("TabRenderer")
            .capture_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.modifiers.control && matches!(event.keystroke.key.as_str(), "w" | "tab" | "p") {
                    return;
                }
                view.send(
                    json!({
                        "action": "key",
                        "pressed": true,
                        "key": event.keystroke.key_char.as_deref().unwrap_or(&event.keystroke.key),
                        "modifiers": modifier_mask(event.keystroke.modifiers)
                    }),
                    cx,
                );
                cx.stop_propagation();
            }))
            .on_key_up(cx.listener(|view, event: &gpui::KeyUpEvent, _, cx| {
                view.send(
                    json!({
                        "action": "key",
                        "pressed": false,
                        "key": event.keystroke.key_char.as_deref().unwrap_or(&event.keystroke.key),
                        "modifiers": modifier_mask(event.keystroke.modifiers)
                    }),
                    cx,
                );
                cx.stop_propagation();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|view, event: &gpui::MouseDownEvent, window, cx| {
                view.focus.focus(window, cx);
                view.pointer(event.position, event.button, true, event.click_count, event.modifiers, false, cx);
                cx.stop_propagation();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(|view, event: &gpui::MouseDownEvent, window, cx| {
                view.focus.focus(window, cx);
                view.pointer(event.position, event.button, true, event.click_count, event.modifiers, false, cx);
                cx.stop_propagation();
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|view, event: &gpui::MouseUpEvent, _, cx| {
                view.pointer(event.position, event.button, false, 1, event.modifiers, false, cx);
            }))
            .on_mouse_up(MouseButton::Right, cx.listener(|view, event: &gpui::MouseUpEvent, _, cx| {
                view.pointer(event.position, event.button, false, 1, event.modifiers, false, cx);
            }))
            .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, _, cx| {
                view.pointer(event.position, event.pressed_button.unwrap_or(MouseButton::Left), event.pressed_button.is_some(), 0, event.modifiers, true, cx);
            }))
            .on_scroll_wheel(cx.listener(|view, event: &gpui::ScrollWheelEvent, _, cx| {
                if let Some(bounds) = view.bounds {
                    let position = view.surface_position(event.position, bounds);
                    let delta = match event.delta {
                        ScrollDelta::Pixels(delta) => delta.map(|value| f32::from(value) / 40.0),
                        ScrollDelta::Lines(delta) => delta,
                    };
                    view.send(
                        json!({
                            "action": "scroll",
                            "x": position.x,
                            "y": position.y,
                            "delta_x": -delta.x,
                            "delta_y": -delta.y,
                            "modifiers": modifier_mask(event.modifiers)
                        }),
                        cx,
                    );
                    cx.stop_propagation();
                }
            }))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        view.update(cx, |view, cx| {
                            if !view.active {
                                view.active = true;
                                view.send(json!({ "action": "active", "active": true }), cx);
                            }
                            if view.bounds.is_none_or(|previous| previous.size != bounds.size) {
                                view.send(
                                    json!({
                                        "action": "resize",
                                        "width": (f32::from(bounds.size.width) * window.scale_factor()).round() as u32,
                                        "height": (f32::from(bounds.size.height) * window.scale_factor()).round() as u32,
                                        "scale": window.scale_factor()
                                    }),
                                    cx,
                                );
                            }
                            view.bounds = Some(bounds);
                        });
                    },
                    move |bounds, _, window, _| {
                        if let Some(frame) = frame {
                            window.paint_image(bounds, bounds, Default::default(), frame, 0, false).log_err();
                        }
                    },
                )
                .size_full(),
            )
            .when_some(self.error.clone(), |element, error| {
                element.child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .p_3()
                        .bg(cx.theme().colors().editor_background)
                        .child(Label::new(error).color(Color::Error)),
                )
            })
            .when(self.frame.is_none() && self.error.is_none(), |element| {
                element.child(div().absolute().top_0().left_0().p_3().child("Loading renderer…"))
            })
    }
}

fn modifier_mask(modifiers: Modifiers) -> u32 {
    u32::from(modifiers.shift)
        | (u32::from(modifiers.control) << 2)
        | (u32::from(modifiers.alt) << 3)
        | (u32::from(modifiers.platform) << 26)
}

fn button_number(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        _ => 1,
    }
}

async fn run_command_backend(
    binary: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    input: Receiver<Value>,
    output: Sender<BackendEvent>,
) -> Result<()> {
    let mut cmd = smol::process::Command::new(&binary);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);

    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "Could not launch tab renderer binary '{}'. Please ensure the extension and companion binary are installed.",
            binary.display()
        )
    })?;

    let mut stdin = child.stdin.take().context("Backend process has no stdin")?;
    let stdout = child.stdout.take().context("Backend process has no stdout")?;

    let writer = async move {
        while let Ok(message) = input.recv().await {
            stdin.write_all(format!("{message}\n").as_bytes()).await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    let reader = async {
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines.next().await {
            let message: Value = serde_json::from_str(&line?)?;
            let event = match message["event"].as_str() {
                Some("frame") => {
                    let png = STANDARD
                        .decode(message["png"].as_str().context("Missing frame PNG")?)?;
                    let mut pixels = image::load_from_memory(&png)?.to_rgba8();
                    for pixel in pixels.chunks_exact_mut(4) {
                        pixel.swap(0, 2);
                    }
                    BackendEvent::Frame(Arc::new(RenderImage::new(smallvec::smallvec![
                        image::Frame::new(pixels)
                    ])))
                }
                Some("modified") => {
                    BackendEvent::Modified(message["modified"].as_bool().unwrap_or(false))
                }
                Some("saved") => {
                    BackendEvent::Saved(message["modified"].as_bool().unwrap_or(false))
                }
                Some("saveError") => BackendEvent::SaveError(
                    message["message"]
                        .as_str()
                        .unwrap_or("Could not save document")
                        .into(),
                ),
                Some("error") => BackendEvent::Error(
                    message["message"]
                        .as_str()
                        .unwrap_or("Backend error")
                        .into(),
                ),
                _ => continue,
            };
            output
                .send(event)
                .await
                .map_err(|error| anyhow!("Tab renderer closed: {error}"))?;
        }
        let status = child.status().await?;
        Err::<(), _>(anyhow!("Tab renderer backend process exited ({status})"))
    };

    match futures::future::select(reader.boxed(), writer.boxed()).await {
        futures::future::Either::Left((result, _))
        | futures::future::Either::Right((result, _)) => result,
    }
}

pub(super) fn init(cx: &mut App) {
    workspace::register_project_item::<TabRendererView>(cx);
}
