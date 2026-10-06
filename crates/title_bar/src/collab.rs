use std::rc::Rc;

use call::ActiveCall;
use gpui::{App, ScreenCaptureSource, Task, Window};
use workspace::notifications::DetachAndPromptErr;

pub fn toggle_screen_sharing(
    screen: anyhow::Result<Option<Rc<dyn ScreenCaptureSource>>>,
    window: &mut Window,
    cx: &mut App,
) {
    let call = ActiveCall::global(cx).read(cx);
    let toggle_screen_sharing = match screen {
        Ok(screen) => {
            let Some(room) = call.room().cloned() else {
                return;
            };

            room.update(cx, |room, cx| {
                let clicked_on_currently_shared_screen =
                    room.shared_screen_id().is_some_and(|screen_id| {
                        Some(screen_id)
                            == screen
                                .as_deref()
                                .and_then(|s| s.metadata().ok().map(|meta| meta.id))
                    });
                let should_unshare_current_screen = room.is_sharing_screen();
                let unshared_current_screen = should_unshare_current_screen.then(|| {
                    telemetry::event!(
                        "Screen Share Disabled",
                        room_id = room.id(),
                        channel_id = room.channel_id(),
                    );
                    room.unshare_screen(clicked_on_currently_shared_screen || screen.is_none(), cx)
                });
                if let Some(screen) = screen {
                    if !should_unshare_current_screen {
                        telemetry::event!(
                            "Screen Share Enabled",
                            room_id = room.id(),
                            channel_id = room.channel_id(),
                        );
                    }
                    cx.spawn(async move |room, cx| {
                        unshared_current_screen.transpose()?;
                        if !clicked_on_currently_shared_screen {
                            room.update(cx, |room, cx| room.share_screen(screen, cx))?
                                .await
                        } else {
                            Ok(())
                        }
                    })
                } else {
                    Task::ready(Ok(()))
                }
            })
        }
        Err(e) => Task::ready(Err(e)),
    };
    toggle_screen_sharing.detach_and_prompt_err("Sharing Screen Failed", window, cx, |e, _, _| Some(format!("{:?}\n\nPlease check that you have given Zed permissions to record your screen in Settings.", e)));
}

pub fn toggle_mute(cx: &mut App) {
    let call = ActiveCall::global(cx).read(cx);
    if let Some(room) = call.room().cloned() {
        room.update(cx, |room, cx| {
            let operation = if room.is_muted() {
                "Microphone Enabled"
            } else {
                "Microphone Disabled"
            };
            telemetry::event!(
                operation,
                room_id = room.id(),
                channel_id = room.channel_id(),
            );

            room.toggle_mute(cx)
        });
    }
}

pub fn toggle_deafen(cx: &mut App) {
    if let Some(room) = ActiveCall::global(cx).read(cx).room().cloned() {
        room.update(cx, |room, cx| room.toggle_deafen(cx));
    }
}
