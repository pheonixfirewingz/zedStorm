mod calculator;

use calculator::{Calculator, Command, Operation, format_number};
use gpui::{
    Action, App, AsyncWindowContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, KeyDownEvent, Pixels, Render, WeakEntity, Window, actions, px,
};
use ui::{
    Button, ButtonLike, ButtonStyle, ElevationIndex, IconName, Label, LabelSize, TintColor,
    prelude::*,
};
use workspace::{
    Panel, Workspace,
    dock::{DockPosition, PanelEvent},
};

actions!(
    calculator,
    [
        ToggleFocus,
        Clear,
        ClearEntry,
        Backspace,
        Equals,
        Negate,
        CopyResult
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<CalculatorPanel>(window, cx);
        });
    })
    .detach();
}

pub struct CalculatorPanel {
    calculator: Calculator,
    focus_handle: FocusHandle,
    position: DockPosition,
    show_history: bool,
}

impl CalculatorPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |_, _, cx| {
            cx.new(|cx| Self {
                calculator: Calculator::default(),
                focus_handle: cx.focus_handle(),
                position: DockPosition::Right,
                show_history: false,
            })
        })
    }

    fn press(&mut self, command: Command, cx: &mut Context<Self>) {
        self.calculator.press(command);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if (modifiers.control || modifiers.platform)
            && event.keystroke.key == "c"
            && self.calculator.error.is_none()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(self.calculator.input.clone()));
            cx.stop_propagation();
            window.prevent_default();
            return;
        }
        if modifiers.control || modifiers.platform || modifiers.alt {
            return;
        }
        if let Some(command) = command_for_key(
            &event.keystroke.key,
            event.keystroke.key_char.as_deref(),
            self.focus_handle.is_focused(window),
        ) {
            self.press(command, cx);
            cx.stop_propagation();
            window.prevent_default();
        }
    }

    fn key_button(
        &self,
        label: &'static str,
        description: &'static str,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ButtonLike {
        Button::new(label, label)
            .full_width()
            .label_size(LabelSize::Large)
            .aria_label(description)
            .tab_index(0_isize)
            .layer(
                if matches!(
                    command,
                    Command::Digit(_) | Command::Decimal | Command::Negate
                ) {
                    ElevationIndex::ElevatedSurface
                } else {
                    ElevationIndex::Surface
                },
            )
            .style(if label == "=" {
                ButtonStyle::Tinted(TintColor::Accent)
            } else if matches!(
                command,
                Command::Digit(_) | Command::Decimal | Command::Negate
            ) {
                ButtonStyle::Filled
            } else {
                ButtonStyle::Outlined
            })
            .on_click(cx.listener(move |this, _, _, cx| this.press(command, cx)))
            .render(window, cx)
            .height(px(48.0).into())
    }
}

fn command_for_key(key: &str, key_character: Option<&str>, enter_equals: bool) -> Option<Command> {
    let key = if matches!(key, "enter" | "escape" | "delete" | "backspace" | "f9") {
        key
    } else {
        key_character.unwrap_or(key)
    };
    Some(match key {
        "0" => Command::Digit('0'),
        "1" => Command::Digit('1'),
        "2" => Command::Digit('2'),
        "3" => Command::Digit('3'),
        "4" => Command::Digit('4'),
        "5" => Command::Digit('5'),
        "6" => Command::Digit('6'),
        "7" => Command::Digit('7'),
        "8" => Command::Digit('8'),
        "9" => Command::Digit('9'),
        "." | "," => Command::Decimal,
        "+" => Command::Binary(Operation::Add),
        "-" => Command::Binary(Operation::Subtract),
        "*" => Command::Binary(Operation::Multiply),
        "/" => Command::Binary(Operation::Divide),
        "%" => Command::Percent,
        "=" => Command::Equals,
        "enter" if enter_equals => Command::Equals,
        "escape" => Command::Clear,
        "delete" => Command::ClearEntry,
        "backspace" => Command::Backspace,
        "f9" => Command::Negate,
        _ => return None,
    })
}

impl Focusable for CalculatorPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for CalculatorPanel {}

impl Panel for CalculatorPanel {
    fn persistent_name() -> &'static str {
        "Calculator"
    }

    fn panel_key() -> &'static str {
        "calculator_panel"
    }

    fn position(&self, _: &Window, _: &App) -> DockPosition {
        self.position
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _: &mut Window, cx: &mut Context<Self>) {
        if self.position_is_valid(position) {
            self.position = position;
            cx.notify();
        }
    }

    fn default_size(&self, _: &Window, _: &App) -> Pixels {
        px(340.0)
    }

    fn min_size(&self, _: &Window, _: &App) -> Option<Pixels> {
        Some(px(280.0))
    }

    fn icon(&self, _: &Window, _: &App) -> Option<IconName> {
        Some(IconName::Calculator)
    }

    fn icon_tooltip(&self, _: &Window, _: &App) -> Option<&'static str> {
        Some("Calculator")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        20
    }
}

impl Render for CalculatorPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = [
            [
                ("%", "Percent", Command::Percent),
                ("CE", "Clear entry (Delete)", Command::ClearEntry),
                ("C", "Clear (Escape)", Command::Clear),
                ("⌫", "Backspace", Command::Backspace),
            ],
            [
                ("1/x", "Reciprocal", Command::Reciprocal),
                ("x²", "Square", Command::Square),
                ("√x", "Square root", Command::SquareRoot),
                ("÷", "Divide", Command::Binary(Operation::Divide)),
            ],
            [
                ("7", "7", Command::Digit('7')),
                ("8", "8", Command::Digit('8')),
                ("9", "9", Command::Digit('9')),
                ("×", "Multiply", Command::Binary(Operation::Multiply)),
            ],
            [
                ("4", "4", Command::Digit('4')),
                ("5", "5", Command::Digit('5')),
                ("6", "6", Command::Digit('6')),
                ("−", "Subtract", Command::Binary(Operation::Subtract)),
            ],
            [
                ("1", "1", Command::Digit('1')),
                ("2", "2", Command::Digit('2')),
                ("3", "3", Command::Digit('3')),
                ("+", "Add", Command::Binary(Operation::Add)),
            ],
            [
                ("+/−", "Change sign (F9)", Command::Negate),
                ("0", "0", Command::Digit('0')),
                (".", "Decimal point", Command::Decimal),
                ("=", "Equals (Enter)", Command::Equals),
            ],
        ];
        let keypad = v_flex()
            .flex_shrink_0()
            .gap_1()
            .children(rows.into_iter().map(|row| {
                h_flex()
                    .gap_1()
                    .children(row.into_iter().map(|(label, description, command)| {
                        div().flex_1().min_w_0().child(self.key_button(
                            label,
                            description,
                            command,
                            window,
                            cx,
                        ))
                    }))
            }));
        let memory = h_flex().flex_shrink_0().gap_1().children(
            [
                ("MC", "Clear memory", Command::MemoryClear),
                ("MR", "Recall memory", Command::MemoryRecall),
                ("M+", "Add to memory", Command::MemoryAdd),
                ("M−", "Subtract from memory", Command::MemorySubtract),
                ("MS", "Store in memory", Command::MemoryStore),
            ]
            .into_iter()
            .map(|(label, description, command)| {
                div().flex_1().min_w_0().child(
                    Button::new(label, label)
                        .full_width()
                        .aria_label(description)
                        .tab_index(0_isize)
                        .disabled(
                            (matches!(command, Command::MemoryClear | Command::MemoryRecall)
                                && self.calculator.memory.is_none())
                                || (self.calculator.error.is_some()
                                    && !matches!(command, Command::MemoryClear)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.press(command, cx))),
                )
            }),
        );

        let history =
            self.show_history.then(|| {
                v_flex()
                    .gap_2()
                    .mt_3()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(Label::new("History"))
                            .child(
                                Button::new("clear-history", "Clear history")
                                    .tab_index(0_isize)
                                    .disabled(self.calculator.history.is_empty())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.calculator.history.clear();
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(self.calculator.history.is_empty(), |this| {
                        this.child(Label::new("No calculations yet").color(Color::Muted))
                    })
                    .children(self.calculator.history.iter().enumerate().rev().map(
                        |(index, entry)| {
                            v_flex()
                                .gap_1()
                                .items_end()
                                .p_2()
                                .rounded_md()
                                .bg(cx.theme().colors().element_background)
                                .child(
                                    Label::new(entry.expression.clone())
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Button::new(("history", index), entry.result.clone())
                                        .aria_label(format!("Recall {}", entry.result))
                                        .tab_index(0_isize)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.calculator.recall_history(index);
                                            cx.notify();
                                        })),
                                )
                        },
                    ))
            });
        v_flex()
            .id("calculator-panel")
            .key_context("CalculatorPanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Clear, _, cx| this.press(Command::Clear, cx)))
            .on_action(
                cx.listener(|this, _: &ClearEntry, _, cx| this.press(Command::ClearEntry, cx)),
            )
            .on_action(cx.listener(|this, _: &Backspace, _, cx| this.press(Command::Backspace, cx)))
            .on_action(cx.listener(|this, _: &Negate, _, cx| this.press(Command::Negate, cx)))
            .on_action(cx.listener(|this, _: &Equals, window, cx| {
                if this.focus_handle.is_focused(window) {
                    this.press(Command::Equals, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CopyResult, _, cx| {
                if this.calculator.error.is_none() {
                    cx.write_to_clipboard(ClipboardItem::new_string(this.calculator.input.clone()));
                }
            }))
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                }),
            )
            .size_full()
            .min_w_0()
            .overflow_y_scroll()
            .p_3()
            .gap_2()
            .bg(cx.theme().colors().panel_background)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .justify_between()
                    .gap_2()
                    .child(
                        v_flex()
                            .child(
                                Label::new("Calculator")
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .child(Label::new("Standard").size(LabelSize::Large)),
                    )
                    .child(
                        Button::new("toggle-history", "History")
                            .tab_index(0_isize)
                            .toggle_state(self.show_history)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_history = !this.show_history;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .items_end()
                    .gap_2()
                    .py_3()
                    .child(
                        Label::new(if self.calculator.expression.is_empty() {
                            " ".into()
                        } else {
                            self.calculator.expression.clone()
                        })
                        .truncate()
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(56.0))
                            .flex_shrink_0()
                            .overflow_hidden()
                            .text_right()
                            .text_size(px(if self.calculator.display().len() > 20 {
                                18.0
                            } else if self.calculator.display().len() > 14 {
                                24.0
                            } else {
                                40.0
                            }))
                            .child(self.calculator.display().to_owned()),
                    )
                    .child(
                        Button::new("copy-result", "Copy result")
                            .tab_index(0_isize)
                            .disabled(self.calculator.error.is_some())
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    this.calculator.input.clone(),
                                ));
                            })),
                    ),
            )
            .child(
                Label::new(
                    self.calculator
                        .memory
                        .map(|memory| format!("M: {}", format_number(memory)))
                        .unwrap_or_else(|| " ".into()),
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(memory)
            .child(keypad)
            .children(history)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn keyboard_input_reaches_the_calculator(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            cx.bind_keys([
                gpui::KeyBinding::new("escape", Clear, Some("CalculatorPanel")),
                gpui::KeyBinding::new("enter", Equals, Some("CalculatorPanel")),
                gpui::KeyBinding::new("backspace", Backspace, Some("CalculatorPanel")),
            ]);
        });
        let (panel, cx) = cx.add_window_view(|_, cx| CalculatorPanel {
            calculator: Calculator::default(),
            focus_handle: cx.focus_handle(),
            position: DockPosition::Right,
            show_history: false,
        });
        panel.update_in(cx, |panel, window, cx| panel.focus_handle.focus(window, cx));
        cx.simulate_keystrokes("2 0 0 + 1 0 % enter");
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.calculator.input.clone()),
            "220"
        );
        cx.simulate_keystrokes("escape 1 2 backspace");
        assert_eq!(
            panel.read_with(cx, |panel, _| panel.calculator.input.clone()),
            "1"
        );
    }

    #[test]
    fn special_keys_take_priority_over_control_characters() {
        assert!(matches!(
            command_for_key("escape", Some("\u{1b}"), true),
            Some(Command::Clear)
        ));
        assert!(matches!(
            command_for_key("enter", Some("\r"), true),
            Some(Command::Equals)
        ));
        assert!(matches!(
            command_for_key("backspace", Some("\u{8}"), true),
            Some(Command::Backspace)
        ));
        assert!(matches!(
            command_for_key("delete", Some("\u{7f}"), true),
            Some(Command::ClearEntry)
        ));
    }

    #[test]
    fn shifted_characters_and_decimal_comma_are_supported() {
        assert!(matches!(
            command_for_key("=", Some("+"), true),
            Some(Command::Binary(Operation::Add))
        ));
        assert!(matches!(
            command_for_key("5", Some("%"), true),
            Some(Command::Percent)
        ));
        assert!(matches!(
            command_for_key(",", Some(","), true),
            Some(Command::Decimal)
        ));
        assert!(matches!(
            command_for_key("1", Some("1"), true),
            Some(Command::Digit('1'))
        ));
    }

    #[test]
    fn enter_is_left_to_focused_buttons_and_other_keys_propagate() {
        assert!(command_for_key("enter", Some("\r"), false).is_none());
        assert!(command_for_key("tab", None, true).is_none());
        assert!(command_for_key("left", None, true).is_none());
    }
}
