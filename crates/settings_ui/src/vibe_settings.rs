use gpui::{Context, Entity, Render, Task, Window};
use ui::{Button, ButtonStyle, Color, Label, LabelSize, prelude::*};
use ui_input::InputField;
use util::ResultExt as _;

#[derive(Clone, PartialEq)]
pub(super) struct VibeApiKey;

pub(super) struct VibeKeyControl {
    input: Entity<InputField>,
    status: String,
    error: bool,
    busy: bool,
    has_key: bool,
    _load_task: Task<()>,
}

impl VibeKeyControl {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputField::new(window, cx, "Enter Mistral API key")
                .masked(true)
                .tab_index(0_isize)
        });
        let credentials = cx.read_credentials(ai::VIBE_CREDENTIAL_KEY);
        let task = cx.spawn(async move |control, cx| {
            let result = credentials.await;
            control
                .update(cx, |control, cx| {
                    control.busy = false;
                    match result {
                        Ok(credentials) => {
                            control.has_key = credentials.is_some();
                            control.status = if control.has_key {
                                "API key saved"
                            } else {
                                "No API key saved"
                            }
                            .into();
                        }
                        Err(error) => {
                            control.error = true;
                            control.status = format!("Could not read API key: {error}");
                        }
                    }
                    cx.notify();
                })
                .log_err();
        });
        Self {
            input,
            status: "Loading saved key…".into(),
            error: false,
            busy: true,
            has_key: false,
            _load_task: task,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let key = self.input.read(cx).text(cx).trim().to_owned();
        if key.is_empty() {
            self.error = true;
            self.status = "Enter an API key before saving.".into();
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = false;
        self.status = "Saving API key…".into();
        let credentials = cx.write_credentials(ai::VIBE_CREDENTIAL_KEY, "mistral", key.as_bytes());
        cx.spawn_in(window, async move |control, cx| {
            let result = credentials.await;
            match &result {
                Ok(()) => {
                    cx.update(|_, cx| cx.set_global(ai::VibeCredentialsChanged))
                        .log_err();
                }
                Err(error) => log::error!("Could not save Vibe API key: {error}"),
            }
            control
                .update_in(cx, |control, window, cx| {
                    control.busy = false;
                    match result {
                        Ok(()) => {
                            let editor = control.input.read(cx).editor().clone();
                            editor.clear(window, cx);
                            control.has_key = true;
                            control.status = "API key saved".into();
                        }
                        Err(error) => {
                            control.error = true;
                            control.status = format!("Could not save API key: {error}");
                        }
                    }
                    cx.notify();
                })
                .log_err();
        })
        .detach();
        cx.notify();
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.has_key {
            return;
        }
        self.busy = true;
        self.error = false;
        self.status = "Removing API key…".into();
        let credentials = cx.delete_credentials(ai::VIBE_CREDENTIAL_KEY);
        cx.spawn(async move |control, cx| {
            let result = credentials.await;
            match &result {
                Ok(()) => cx.update(|cx| cx.set_global(ai::VibeCredentialsChanged)),
                Err(error) => log::error!("Could not remove Vibe API key: {error}"),
            }
            control
                .update(cx, |control, cx| {
                    control.busy = false;
                    match result {
                        Ok(()) => {
                            control.has_key = false;
                            control.status = "API key removed".into();
                        }
                        Err(error) => {
                            control.error = true;
                            control.status = format!("Could not remove API key: {error}");
                        }
                    }
                    cx.notify();
                })
                .log_err();
        })
        .detach();
        cx.notify();
    }
}

impl Render for VibeKeyControl {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .max_w(px(320.))
            .gap_2()
            .child(self.input.clone())
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("save-vibe-key", "Save")
                            .style(ButtonStyle::Outlined)
                            .tab_index(0_isize)
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|control, _, window, cx| control.save(window, cx)),
                            ),
                    )
                    .child(
                        Button::new("remove-vibe-key", "Remove")
                            .tab_index(0_isize)
                            .disabled(self.busy || !self.has_key)
                            .on_click(cx.listener(|control, _, _, cx| control.remove(cx))),
                    ),
            )
            .child(
                Label::new(self.status.clone())
                    .size(LabelSize::Small)
                    .color(if self.error {
                        Color::Error
                    } else {
                        Color::Muted
                    }),
            )
    }
}
