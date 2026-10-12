use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use gtk::{glib, prelude::*};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    io::{BufRead, Write},
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    time::Duration,
};
use webkit2gtk::{SettingsExt, WebViewExt};
use wry::{WebViewBuilder, WebViewBuilderExtUnix, WebViewExtUnix};
use zed_diagrams::Document;

pub fn run(path: PathBuf) -> Result<()> {
    gtk::init()?;
    let document = Rc::new(RefCell::new(Document::open(&path)?));
    let token = uuid::Uuid::new_v4().to_string();
    let html = include_str!("editor.html")
        .replace("/* BRIDGE */", include_str!("bridge.js"))
        .replace("__BRIDGE_TOKEN__", &token);
    let (output, output_receiver) = mpsc::sync_channel::<Value>(4);
    std::thread::spawn(move || {
        let mut stdout = std::io::stdout().lock();
        for message in output_receiver {
            if let Err(error) = serde_json::to_writer(&mut stdout, &message)
                .map_err(std::io::Error::other)
                .and_then(|()| stdout.write_all(b"\n"))
                .and_then(|()| stdout.flush())
            {
                eprintln!("Embedded webview output closed: {error}");
                std::process::exit(0);
            }
        }
    });
    let (input, input_receiver) = mpsc::sync_channel::<Value>(128);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line
                .map_err(anyhow::Error::from)
                .and_then(|line| Ok(serde_json::from_str(&line)?))
            {
                Ok(message) => {
                    if input.send(message).is_err() {
                        return;
                    }
                }
                Err(error) => eprintln!("Invalid embedded webview input: {error}"),
            }
        }
    });
    let container = gtk::OffscreenWindow::new();
    container.set_default_size(320, 200);
    let pending_events = Rc::new(RefCell::new(Vec::<Value>::new()));
    let webview = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler({
            let pending_events = pending_events.clone();
            move |request| match serde_json::from_str::<Value>(request.body()) {
                Ok(message) if message.get("token").and_then(Value::as_str) == Some(&token) => {
                    pending_events.borrow_mut().push(message)
                }
                Ok(_) => eprintln!("Rejected embedded webview token"),
                Err(error) => eprintln!("Invalid embedded webview IPC: {error}"),
            }
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .build_gtk(&container)?;
    let webkit = webview.webview();
    // Offscreen GTK windows have no GL context; let WebKit paint into its software surface.
    if let Some(settings) = webkit2gtk::WebViewExt::settings(&webkit) {
        settings.set_hardware_acceleration_policy(webkit2gtk::HardwareAccelerationPolicy::Never);
    }
    container.show_all();
    webkit.grab_focus();
    let active = Rc::new(Cell::new(true));
    let snapshot_pending = Rc::new(Cell::new(false));
    glib::timeout_add_local(Duration::from_millis(50), {
        let output = output.clone();
        let active = active.clone();
        let webkit = webkit.clone();
        move || {
            loop {
                match input_receiver.try_recv() {
                    Ok(message) => {
                        let result = match message["action"].as_str() {
                            Some("resize") => {
                                let width =
                                    message["width"].as_i64().unwrap_or(1000).clamp(320, 4096)
                                        as i32;
                                let height =
                                    message["height"].as_i64().unwrap_or(700).clamp(200, 4096)
                                        as i32;
                                webkit.set_zoom_level(
                                    message["scale"].as_f64().unwrap_or(1.0).clamp(0.5, 4.0),
                                );
                                webkit.set_size_request(width, height);
                                container.set_default_size(width, height);
                                container.resize(width, height);
                                Ok(())
                            }
                            Some("active") => {
                                active.set(message["active"].as_bool().unwrap_or(false));
                                Ok(())
                            }
                            Some("save") => evaluate(&webview, json!({"action":"requestSave"})),
                            Some("reload") => {
                                let path = document.borrow().path().to_path_buf();
                                match Document::open(&path) {
                                    Ok(reloaded) => {
                                        *document.borrow_mut() = reloaded;
                                        emit(&output, json!({"event":"modified","modified":false}));
                                        evaluate(
                                            &webview,
                                            json!({"action":"load","xml":document.borrow().xml(),"autosave":1,"noExitBtn":true,"saveAndExit":false}),
                                        )
                                    }
                                    Err(error) => Err(error),
                                }
                            }
                            Some("close") => {
                                gtk::main_quit();
                                return glib::ControlFlow::Break;
                            }
                            Some("pointer") | Some("key") | Some("scroll") => {
                                dispatch_input(&webkit, &message)
                            }
                            _ => Ok(()),
                        };
                        if let Err(error) = result {
                            emit(
                                &output,
                                json!({"event":"error","message":error.to_string()}),
                            );
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        gtk::main_quit();
                        return glib::ControlFlow::Break;
                    }
                }
            }
            for message in pending_events.borrow_mut().drain(..) {
                let response = match message["event"].as_str() {
                    Some("init") => Some(
                        json!({"action":"load","xml":document.borrow().xml(),"autosave":1,"noExitBtn":true,"saveAndExit":false}),
                    ),
                    Some("save") => {
                        let result = message["xml"]
                            .as_str()
                            .context("Save has no XML")
                            .and_then(|xml| document.borrow_mut().save(xml));
                        match result {
                            Ok(()) => Some(json!({"action":"saved"})),
                            Err(error) => {
                                emit(
                                    &output,
                                    json!({"event":"saveError","message":error.to_string()}),
                                );
                                Some(json!({"action":"saveError","message":error.to_string()}))
                            }
                        }
                    }
                    Some("modified") => {
                        emit(
                            &output,
                            json!({"event":"modified","modified":message["modified"]}),
                        );
                        None
                    }
                    Some("saved") => {
                        emit(
                            &output,
                            json!({"event":"saved","modified":message["modified"]}),
                        );
                        None
                    }
                    Some("exit") => {
                        emit(&output, json!({"event":"exit"}));
                        None
                    }
                    _ => None,
                };
                if let Some(response) = response
                    && let Err(error) = evaluate(&webview, response)
                {
                    emit(
                        &output,
                        json!({"event":"error","message":error.to_string()}),
                    );
                }
            }
            glib::ControlFlow::Continue
        }
    });
    let last_frame = Rc::new(RefCell::new(None::<Vec<u8>>));
    glib::timeout_add_local(Duration::from_millis(100), move || {
        if active.get() && !snapshot_pending.replace(true) {
            let snapshot_pending = snapshot_pending.clone();
            let output = output.clone();
            let last_frame = last_frame.clone();
            webkit.snapshot(
                webkit2gtk::SnapshotRegion::Visible,
                webkit2gtk::SnapshotOptions::NONE,
                None::<&gtk::gio::Cancellable>,
                move |surface| {
                    snapshot_pending.set(false);
                    let result = surface.map_err(anyhow::Error::from).and_then(|surface| {
                        let mut bytes = Vec::new();
                        surface.write_to_png(&mut bytes)?;
                        Ok(bytes)
                    });
                    match result {
                        Ok(bytes) => {
                            if last_frame.borrow().as_ref() != Some(&bytes) {
                                match output.try_send(
                                    json!({"event":"frame","png":STANDARD.encode(&bytes)}),
                                ) {
                                    Ok(()) => *last_frame.borrow_mut() = Some(bytes),
                                    Err(mpsc::TrySendError::Full(_)) => {}
                                    Err(mpsc::TrySendError::Disconnected(_)) => gtk::main_quit(),
                                }
                            }
                        }
                        Err(error) => eprintln!("Could not snapshot system webview: {error}"),
                    }
                },
            );
        }
        glib::ControlFlow::Continue
    });
    gtk::main();
    Ok(())
}

fn evaluate(webview: &wry::WebView, message: Value) -> Result<()> {
    webview.evaluate_script(&format!("window.receiveNative({message})"))?;
    Ok(())
}

fn emit(output: &mpsc::SyncSender<Value>, message: Value) {
    if let Err(error) = output.send(message) {
        eprintln!("Embedded webview output failed: {error}");
        gtk::main_quit();
    }
}

fn dispatch_input(webview: &webkit2gtk::WebView, message: &Value) -> Result<()> {
    use glib::translate::{IntoGlib, ToGlibPtr};
    let window = webview.window().context("Webview has no input window")?;
    let pressed = message["pressed"].as_bool().unwrap_or(false);
    let kind = match message["action"].as_str() {
        Some("pointer") => match message["kind"].as_str() {
            Some("move") => gdk::EventType::MotionNotify,
            _ if !pressed => gdk::EventType::ButtonRelease,
            _ if message["clicks"].as_u64().unwrap_or(1) >= 2 => gdk::EventType::DoubleButtonPress,
            _ => gdk::EventType::ButtonPress,
        },
        Some("key") if pressed => gdk::EventType::KeyPress,
        Some("key") => gdk::EventType::KeyRelease,
        _ => gdk::EventType::Scroll,
    };
    let mut event = gdk::Event::new(kind);
    event.as_mut().window = window.to_glib_full();
    event.as_mut().send_event = 1;
    let modifiers = message["modifiers"].as_u64().unwrap_or(0) as u32;
    let x = message["x"].as_f64().unwrap_or(0.0);
    let y = message["y"].as_f64().unwrap_or(0.0);
    if let Some(button) = event.downcast_mut::<gdk::EventButton>() {
        let fields = button.as_mut();
        fields.x = x;
        fields.y = y;
        fields.x_root = x;
        fields.y_root = y;
        fields.state = modifiers;
        fields.button = message["button"].as_u64().unwrap_or(1) as u32;
    } else if let Some(motion) = event.downcast_mut::<gdk::EventMotion>() {
        let fields = motion.as_mut();
        fields.x = x;
        fields.y = y;
        fields.x_root = x;
        fields.y_root = y;
        fields.state = modifiers;
    } else if let Some(scroll) = event.downcast_mut::<gdk::EventScroll>() {
        let fields = scroll.as_mut();
        fields.x = x;
        fields.y = y;
        fields.state = modifiers;
        fields.direction = gdk::ScrollDirection::Smooth.into_glib();
        fields.delta_x = message["delta_x"].as_f64().unwrap_or(0.0);
        fields.delta_y = message["delta_y"].as_f64().unwrap_or(0.0);
    } else if let Some(key) = event.downcast_mut::<gdk::EventKey>() {
        let name = message["key"].as_str().unwrap_or("");
        let name = match name {
            "enter" => "Return",
            "backspace" => "BackSpace",
            "delete" => "Delete",
            "escape" => "Escape",
            "tab" => "Tab",
            "space" => "space",
            "left" => "Left",
            "right" => "Right",
            "up" => "Up",
            "down" => "Down",
            "home" => "Home",
            "end" => "End",
            "pageup" => "Page_Up",
            "pagedown" => "Page_Down",
            name => name,
        };
        let keyval = if name.chars().count() == 1 {
            name.chars()
                .next()
                .map(gdk::keys::Key::from_unicode)
                .unwrap_or(gdk::keys::constants::VoidSymbol)
        } else {
            gdk::keys::Key::from_name(name)
        };
        let fields = key.as_mut();
        fields.keyval = *keyval;
        fields.state = modifiers;
        if let Some(keymap) = gdk::Keymap::for_display(&webview.display())
            && let Some(entry) = keymap.entries_for_keyval(*keyval).first()
        {
            fields.hardware_keycode = entry.keycode() as u16;
            fields.group = entry.group() as u8;
        }
    }
    if let Some(seat) = webview.display().default_seat() {
        let device = if message["action"] == "key" {
            seat.keyboard()
        } else {
            seat.pointer()
        };
        event.set_device(device.as_ref());
    }
    webview.grab_focus();
    webview.event(&event);
    Ok(())
}
