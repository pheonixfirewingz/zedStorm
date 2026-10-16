use std::path::PathBuf;
use anyhow::{Context as _, Result};
use editor::Editor;
use gpui::{
    div, px, rems, App, AppContext, ClipboardItem, Context, DismissEvent,
    Entity, EventEmitter, FocusHandle, Focusable, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window,
};
use project::project_settings::ProjectSettings;
use ui::{
    prelude::*, Button, ButtonSize, ButtonStyle, ContextMenu, DropdownMenu, DropdownStyle,
    Icon, IconButton, IconName, IconPosition, IconSize, Label, LabelSize, Tooltip,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpgKeyInfo {
    pub key_id: String,
    pub fingerprint: String,
    pub user_id: String,
}

pub fn find_gpg_binary() -> Option<PathBuf> {
    which::which("gpg")
        .or_else(|_| which::which("gpg2"))
        .or_else(|_| which::which("gpg.exe"))
        .ok()
}

pub async fn ensure_loopback_pinentry() -> Result<()> {
    let gnupg_dir = paths::home_dir().join(".gnupg");
    smol::fs::create_dir_all(&gnupg_dir).await.ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = smol::fs::set_permissions(&gnupg_dir, std::fs::Permissions::from_mode(0o700)).await;
    }
    let agent_conf = gnupg_dir.join("gpg-agent.conf");
    let content = smol::fs::read_to_string(&agent_conf).await.unwrap_or_default();
    if !content.contains("allow-loopback-pinentry") {
        let mut new_content = content;
        if !new_content.is_empty() && !new_content.ends_with('\n') {
            new_content.push('\n');
        }
        new_content.push_str("allow-loopback-pinentry\n");
        smol::fs::write(&agent_conf, new_content)
            .await
            .context("failed to write gpg-agent.conf")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = smol::fs::set_permissions(&agent_conf, std::fs::Permissions::from_mode(0o600)).await;
        }
        let _ = smol::process::Command::new("gpgconf")
            .arg("--reload")
            .arg("gpg-agent")
            .output()
            .await;
    }
    Ok(())
}

pub async fn list_gpg_secret_keys() -> Result<Vec<GpgKeyInfo>> {
    let gpg = find_gpg_binary().context("gpg binary not found")?;
    let output = smol::process::Command::new(gpg)
        .arg("--list-secret-keys")
        .arg("--with-colons")
        .output()
        .await
        .context("failed to run gpg --list-secret-keys")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut keys = Vec::new();
    let mut current_sec: Option<String> = None;
    let mut current_fpr: Option<String> = None;
    let mut current_uid: Option<String> = None;

    for line in stdout.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.is_empty() {
            continue;
        }
        match fields[0] {
            "sec" => {
                if let Some(sec) = current_sec.take() {
                    keys.push(GpgKeyInfo {
                        key_id: sec,
                        fingerprint: current_fpr.take().unwrap_or_default(),
                        user_id: current_uid.take().unwrap_or_else(|| "Unknown User".to_string()),
                    });
                }
                current_sec = fields.get(4).filter(|s| !s.is_empty()).map(|s| s.to_string());
                current_fpr = None;
                current_uid = None;
            }
            "fpr" => {
                if current_fpr.is_none() {
                    current_fpr = fields.get(9).filter(|s| !s.is_empty()).map(|s| s.to_string());
                }
            }
            "uid" => {
                if current_uid.is_none() {
                    current_uid = fields.get(9).filter(|s| !s.is_empty()).map(|s| s.to_string());
                }
            }
            _ => {}
        }
    }

    if let Some(sec) = current_sec {
        keys.push(GpgKeyInfo {
            key_id: sec,
            fingerprint: current_fpr.unwrap_or_default(),
            user_id: current_uid.unwrap_or_else(|| "Unknown User".to_string()),
        });
    }

    Ok(keys)
}

pub async fn export_gpg_public_key(key_id: &str) -> Result<String> {
    let gpg = find_gpg_binary().context("gpg binary not found")?;
    let output = smol::process::Command::new(gpg)
        .arg("--armor")
        .arg("--export")
        .arg(key_id)
        .output()
        .await
        .context("failed to export gpg public key")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gpg --export failed: {stderr}");
    }

    let pubkey = String::from_utf8_lossy(&output.stdout).into_owned();
    if pubkey.trim().is_empty() {
        anyhow::bail!("no public key exported for {key_id}");
    }
    Ok(pubkey)
}

pub async fn generate_gpg_key(
    name: &str,
    email: &str,
    passphrase: &str,
) -> Result<GpgKeyInfo> {
    let gpg = find_gpg_binary().context("gpg binary not found")?;
    ensure_loopback_pinentry().await.ok();

    let user_id = format!("{name} <{email}>");
    let output = smol::process::Command::new(&gpg)
        .arg("--batch")
        .arg("--pinentry-mode")
        .arg("loopback")
        .arg("--passphrase")
        .arg(passphrase)
        .arg("--quick-gen-key")
        .arg(&user_id)
        .arg("default")
        .arg("default")
        .arg("never")
        .output()
        .await
        .context("failed to execute gpg --quick-gen-key")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("GPG key generation failed: {stderr}");
    }

    let _ = smol::process::Command::new("gpgconf")
        .arg("--reload")
        .arg("gpg-agent")
        .output()
        .await;

    let keys = list_gpg_secret_keys().await?;
    let created_key = keys
        .into_iter()
        .find(|k| k.user_id.contains(email) || k.user_id.contains(name))
        .context("Key was generated but could not be located in secret keys")?;

    Ok(created_key)
}

pub struct GenerateGpgModal {
    pub(crate) name_editor: Entity<Editor>,
    pub(crate) email_editor: Entity<Editor>,
    pub(crate) passphrase_editor: Entity<Editor>,
    pub(crate) confirm_passphrase_editor: Entity<Editor>,
    pub(crate) error_message: Option<SharedString>,
    pub(crate) is_generating: bool,
    pub(crate) focus_handle: FocusHandle,
}

impl EventEmitter<DismissEvent> for GenerateGpgModal {}

impl Focusable for GenerateGpgModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl GenerateGpgModal {
    pub fn new(
        name: Option<String>,
        email: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Full Name", window, cx);
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                editor.set_text(name, window, cx);
            }
            editor
        });

        let email_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Email Address", window, cx);
            if let Some(email) = email.filter(|e| !e.is_empty()) {
                editor.set_text(email, window, cx);
            }
            editor
        });

        let passphrase_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Passphrase (optional)", window, cx);
            editor.set_masked(true, cx);
            editor
        });

        let confirm_passphrase_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Confirm Passphrase", window, cx);
            editor.set_masked(true, cx);
            editor
        });

        let focus_handle = cx.focus_handle();

        Self {
            name_editor,
            email_editor,
            passphrase_editor,
            confirm_passphrase_editor,
            error_message: None,
            is_generating: false,
            focus_handle,
        }
    }

    pub fn generate(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.is_generating {
            return;
        }

        let name = self.name_editor.read(cx).text(cx).trim().to_string();
        let email = self.email_editor.read(cx).text(cx).trim().to_string();
        let passphrase = self.passphrase_editor.read(cx).text(cx);
        let confirm_passphrase = self.confirm_passphrase_editor.read(cx).text(cx);

        if name.is_empty() {
            self.error_message = Some("Full Name is required".into());
            cx.notify();
            return;
        }

        if email.is_empty() || !email.contains('@') {
            self.error_message = Some("A valid Email Address is required".into());
            cx.notify();
            return;
        }

        if passphrase != confirm_passphrase {
            self.error_message = Some("Passphrases do not match".into());
            cx.notify();
            return;
        }

        self.error_message = None;
        self.is_generating = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = generate_gpg_key(&name, &email, &passphrase).await;
            match result {
                Ok(key) => {
                    let pubkey = export_gpg_public_key(&key.key_id).await.ok();
                    this.update(cx, |this, cx| {
                        if let Some(pubkey) = pubkey {
                            cx.write_to_clipboard(ClipboardItem::new_string(pubkey));
                        }

                        let key_id = key.key_id.clone();
                        settings::update_settings_file(<dyn fs::Fs>::global(cx), cx, move |settings, _| {
                            let git = settings.git.get_or_insert_default();
                            git.gpg_signing_key = Some(key_id);
                            git.sign_commits = Some(true);
                        });

                        this.is_generating = false;
                        cx.emit(DismissEvent);
                    })
                    .ok();
                }
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.is_generating = false;
                        this.error_message = Some(format!("Error: {err:#}").into());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }
}

impl Render for GenerateGpgModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("GenerateGpgModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| {
                cx.emit(DismissEvent);
            }))
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                this.generate(window, cx);
            }))
            .w(rems(32.0))
            .bg(cx.theme().colors().elevated_surface_background)
            .border_1()
            .border_color(cx.theme().colors().border)
            .rounded_lg()
            .shadow_lg()
            .p_5()
            .gap_4()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(
                                Label::new("Generate GPG Key")
                                    .size(LabelSize::Large)
                                    .weight(FontWeight::BOLD),
                            )
                            .child(
                                IconButton::new("close-gpg-modal", IconName::Close)
                                    .icon_size(IconSize::Small)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(DismissEvent);
                                    })),
                            ),
                    )
                    .child(
                        Label::new(
                            "Create a new GPG key for signing your Git commits. Passphrase will be requested when committing.",
                        )
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    ),
            )
            .child(ui::Divider::horizontal())
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Full Name").size(LabelSize::Small))
                            .child(
                                div()
                                    .border_1()
                                    .border_color(cx.theme().colors().border_variant)
                                    .rounded_md()
                                    .px_2()
                                    .py_1()
                                    .bg(cx.theme().colors().editor_background)
                                    .child(self.name_editor.clone()),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Email Address").size(LabelSize::Small))
                            .child(
                                div()
                                    .border_1()
                                    .border_color(cx.theme().colors().border_variant)
                                    .rounded_md()
                                    .px_2()
                                    .py_1()
                                    .bg(cx.theme().colors().editor_background)
                                    .child(self.email_editor.clone()),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Passphrase (optional)").size(LabelSize::Small))
                            .child(
                                div()
                                    .border_1()
                                    .border_color(cx.theme().colors().border_variant)
                                    .rounded_md()
                                    .px_2()
                                    .py_1()
                                    .bg(cx.theme().colors().editor_background)
                                    .child(self.passphrase_editor.clone()),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new("Confirm Passphrase").size(LabelSize::Small))
                            .child(
                                div()
                                    .border_1()
                                    .border_color(cx.theme().colors().border_variant)
                                    .rounded_md()
                                    .px_2()
                                    .py_1()
                                    .bg(cx.theme().colors().editor_background)
                                    .child(self.confirm_passphrase_editor.clone()),
                            ),
                    ),
            )
            .when_some(self.error_message.as_ref(), |this, err| {
                this.child(
                    Label::new(err.clone())
                        .size(LabelSize::Small)
                        .color(Color::Error),
                )
            })
            .when(self.is_generating, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Icon::new(IconName::RotateCw)
                                .size(IconSize::Small)
                                .color(Color::Accent),
                        )
                        .child(
                            Label::new("Generating GPG key... this may take a few seconds.")
                                .size(LabelSize::Small)
                                .color(Color::Accent),
                        ),
                )
            })
            .child(ui::Divider::horizontal())
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-gpg-modal", "Cancel")
                            .style(ButtonStyle::Subtle)
                            .disabled(self.is_generating)
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(DismissEvent);
                            })),
                    )
                    .child(
                        Button::new("submit-gpg-modal", "Generate Key")
                            .style(ButtonStyle::Filled)
                            .disabled(self.is_generating)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.generate(window, cx);
                            })),
                    ),
            )
    }
}

pub struct GpgSigningSettings {
    pub(crate) keys: Vec<GpgKeyInfo>,
    pub(crate) gpg_binary: Option<PathBuf>,
    pub(crate) is_loading: bool,
    pub(crate) status_message: Option<(SharedString, bool)>,
    pub(crate) manual_mode: bool,
    pub(crate) manual_editor: Entity<Editor>,
}

impl GpgSigningSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let gpg_binary = find_gpg_binary();
        let manual_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Enter Key ID or Fingerprint", window, cx);
            editor
        });

        let mut this = Self {
            keys: Vec::new(),
            gpg_binary,
            is_loading: false,
            status_message: None,
            manual_mode: false,
            manual_editor,
        };

        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.gpg_binary.is_none() {
            return;
        }

        self.is_loading = true;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let keys = list_gpg_secret_keys().await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.keys = keys;
                this.is_loading = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn copy_public_key(&mut self, key_id: String, cx: &mut Context<Self>) {
        self.status_message = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            match export_gpg_public_key(&key_id).await {
                Ok(pubkey) => {
                    this.update(cx, |this, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(pubkey));
                        this.status_message = Some((
                            "GPG public key copied to clipboard! Ready to paste into GitHub/GitLab."
                                .into(),
                            false,
                        ));
                        cx.notify();
                    })
                    .ok();
                }
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.status_message = Some((
                            format!("Failed to export public key: {err:#}").into(),
                            true,
                        ));
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }
}

impl Render for GpgSigningSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current_key_id = cx
            .global::<settings::SettingsStore>()
            .get::<ProjectSettings>(None)
            .git
            .gpg_signing_key
            .clone();

        let matched_key = current_key_id.as_ref().and_then(|id| {
            self.keys
                .iter()
                .find(|k| &k.key_id == id || &k.fingerprint == id)
        });

        let current_display_label = if let Some(key) = matched_key {
            let email_or_uid = if let Some(start) = key.user_id.find('<') {
                if let Some(end) = key.user_id.rfind('>') {
                    &key.user_id[start + 1..end]
                } else {
                    &key.user_id
                }
            } else {
                &key.user_id
            };
            format!("{} ({})", key.key_id, email_or_uid)
        } else if let Some(ref id) = current_key_id {
            format!("Custom: {id}")
        } else {
            "No GPG key selected".to_string()
        };

        let keys = self.keys.clone();
        let selected_key_id = current_key_id.clone();
        let this_entity = cx.entity().clone();

        v_flex()
            .items_end()
            .gap_2()
            .when(self.gpg_binary.is_none(), |this| {
                this.child(
                    Label::new("GPG not found on PATH. Install GnuPG to use commit signing.")
                        .color(Color::Warning)
                        .size(LabelSize::Small),
                )
            })
            .when(self.gpg_binary.is_some(), |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .justify_end()
                        .when(!self.manual_mode, |this| {
                            let current_display = current_display_label.clone();
                            let keys_for_menu = keys.clone();
                            let selected_for_menu = selected_key_id.clone();
                            let this_entity_custom = this_entity.clone();

                            this.child(
                                DropdownMenu::new(
                                    "gpg-key-dropdown",
                                    current_display,
                                    ContextMenu::build(_window, cx, move |mut menu, _, _| {
                                        for key in &keys_for_menu {
                                            let key_id = key.key_id.clone();
                                            let is_sel = selected_for_menu.as_ref() == Some(&key.key_id)
                                                || selected_for_menu.as_ref() == Some(&key.fingerprint);
                                            let label = format!("{} ({})", key.key_id, key.user_id);
                                            let key_id_clone = key_id.clone();

                                            menu = menu.toggleable_entry(
                                                label,
                                                is_sel,
                                                IconPosition::End,
                                                None,
                                                move |_, cx| {
                                                    let key_id = key_id_clone.clone();
                                                    settings::update_settings_file(
                                                        <dyn fs::Fs>::global(cx),
                                                        cx,
                                                        move |settings, _| {
                                                            let git = settings.git.get_or_insert_default();
                                                            git.gpg_signing_key = Some(key_id);
                                                            git.sign_commits = Some(true);
                                                        },
                                                    );
                                                },
                                            );
                                        }

                                        if !keys_for_menu.is_empty() {
                                            menu = menu.separator();
                                        }

                                        let is_none_sel = selected_for_menu.is_none();
                                        menu = menu.toggleable_entry(
                                            "None (Disable GPG Signing)",
                                            is_none_sel,
                                            IconPosition::End,
                                            None,
                                            move |_, cx| {
                                                settings::update_settings_file(
                                                    <dyn fs::Fs>::global(cx),
                                                    cx,
                                                    move |settings, _| {
                                                        let git = settings.git.get_or_insert_default();
                                                        git.gpg_signing_key = None;
                                                    },
                                                );
                                            },
                                        );

                                        let this_entity_inner = this_entity_custom.clone();
                                        menu = menu.entry(
                                            "Custom Key ID...",
                                            None,
                                            move |_, cx| {
                                                this_entity_inner.update(cx, |this, cx| {
                                                    this.manual_mode = true;
                                                    cx.notify();
                                                });
                                            },
                                        );

                                        menu
                                    }),
                                )
                                .trigger_size(ButtonSize::Medium)
                                .style(DropdownStyle::Outlined)
                                .offset(gpui::Point {
                                    x: px(0.0),
                                    y: px(2.0),
                                }),
                            )
                        })
                        .when(self.manual_mode, |this| {
                            let manual_editor = self.manual_editor.clone();
                            this.child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .w(rems(16.0))
                                            .border_1()
                                            .border_color(cx.theme().colors().border_variant)
                                            .rounded_md()
                                            .px_2()
                                            .py_1()
                                            .bg(cx.theme().colors().editor_background)
                                            .child(manual_editor),
                                    )
                                    .child(
                                        Button::new("confirm-manual-gpg", "Save")
                                            .size(ButtonSize::Compact)
                                            .style(ButtonStyle::Filled)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                let text = this.manual_editor.read(cx).text(cx).trim().to_string();
                                                let key_opt = if text.is_empty() { None } else { Some(text) };
                                                this.manual_mode = false;
                                                settings::update_settings_file(
                                                    <dyn fs::Fs>::global(cx),
                                                    cx,
                                                    move |settings, _| {
                                                        let git = settings.git.get_or_insert_default();
                                                        git.gpg_signing_key = key_opt;
                                                        git.sign_commits = Some(true);
                                                    },
                                                );
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        Button::new("cancel-manual-gpg", "Cancel")
                                            .size(ButtonSize::Compact)
                                            .style(ButtonStyle::Subtle)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.manual_mode = false;
                                                cx.notify();
                                            })),
                                    ),
                            )
                        })
                        .child(
                            IconButton::new("refresh-gpg-btn", IconName::RotateCw)
                                .icon_size(IconSize::Small)
                                .tooltip(Tooltip::text("Refresh GPG Keys"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.refresh(cx);
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .justify_end()
                        .child(
                            Button::new("generate-gpg-key-btn", "Generate Key")
                                .start_icon(Icon::new(IconName::Plus))
                                .size(ButtonSize::Medium)
                                .style(ButtonStyle::Filled)
                                .on_click(cx.listener(|_, _, window, cx| {
                                    window.dispatch_action(Box::new(crate::OpenGenerateGpgModalAction), cx);
                                })),
                        )
                        .when_some(current_key_id.clone(), |this, key_id| {
                            this.child(
                                Button::new("copy-gpg-pubkey-btn", "Copy Public Key")
                                    .start_icon(Icon::new(IconName::Copy))
                                    .size(ButtonSize::Medium)
                                    .style(ButtonStyle::OutlinedGhost)
                                    .tooltip(Tooltip::text("Copy ASCII-armored public key to clipboard"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.copy_public_key(key_id.clone(), cx);
                                    })),
                            )
                        }),
                )
            })
            .when_some(self.status_message.as_ref(), |this, (msg, is_error)| {
                this.child(
                    Label::new(msg.clone())
                        .size(LabelSize::Small)
                        .color(if *is_error { Color::Error } else { Color::Success }),
                )
            })
    }
}
