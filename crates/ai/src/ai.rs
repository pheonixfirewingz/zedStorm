mod codex_panel;
mod codex_protocol;

use anyhow::Context as _;
use gpui::{App, AppContext as _, Context, Window};
use workspace::{Workspace, notifications::DetachAndPromptErr};

pub use context_mcp::serve as serve_context_mcp;

pub fn init(cx: &mut App) {
    command_palette_hooks::CommandPaletteFilter::update_global(cx, |filter, _| {
        for namespace in [
            "acp",
            "agent",
            "agents",
            "agents_sidebar",
            "assistant",
            "assistant2",
            "bedrock",
            "context_server",
            "copilot",
            "edit_prediction",
            "inline_assistant",
            "zeta",
            "zed_predict_onboarding",
        ] {
            filter.hide_namespace(namespace);
        }
        filter.hide_action_types(&[
            std::any::TypeId::of::<zed_actions::OpenZedPredictOnboarding>(),
            std::any::TypeId::of::<editor::actions::AcceptEditPrediction>(),
            std::any::TypeId::of::<editor::actions::AcceptNextWordEditPrediction>(),
            std::any::TypeId::of::<editor::actions::AcceptNextLineEditPrediction>(),
            std::any::TypeId::of::<editor::actions::NextEditPrediction>(),
            std::any::TypeId::of::<editor::actions::PreviousEditPrediction>(),
            std::any::TypeId::of::<editor::actions::ShowEditPrediction>(),
            std::any::TypeId::of::<editor::actions::ToggleEditPrediction>(),
            std::any::TypeId::of::<editor::actions::SendReviewToAgent>(),
        ]);
    });
}

pub fn init_workspace(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    workspace.register_action(open_codex);
    let panel = cx.new(|cx| codex_panel::CodexPanel::new(workspace, window, cx));
    workspace.add_panel(panel, window, cx);
}

fn open_codex(
    workspace: &mut Workspace,
    _: &zed_actions::OpenCodex,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let panels_task = workspace.take_panels_task();
    cx.spawn_in(window, async move |workspace, cx| {
        if let Some(panels_task) = panels_task {
            panels_task.await?;
        }
        workspace.update_in(cx, |workspace, window, cx| {
            let is_visible = workspace.all_docks().iter().any(|dock| {
                dock.read(cx).visible_panel().is_some_and(|panel| {
                    panel.to_any().downcast::<codex_panel::CodexPanel>().is_ok()
                })
            });
            if is_visible {
                workspace.close_panel::<codex_panel::CodexPanel>(window, cx);
                workspace.focus_center_pane(window, cx);
                return Ok::<(), anyhow::Error>(());
            }
            workspace
                .focus_panel::<codex_panel::CodexPanel>(window, cx)
                .context("Codex chat panel is unavailable")?;
            Ok(())
        })??;
        Ok(())
    })
    .detach_and_prompt_err("Could not open Codex chat", window, cx, |_, _, _| {
        Some("Install Codex CLI and ensure `codex` is on your PATH.".into())
    });
}
