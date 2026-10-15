use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageEncoder as _;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    ffi::c_void,
    io::{BufRead, Write},
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::windows::WindowExtWindows,
    window::WindowBuilder,
};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use wry::WebViewBuilder;
use zed_diagrams::Document;

unsafe extern "system" {
    fn PrintWindow(hwnd: *mut c_void, hdc_blt: *mut c_void, n_flags: u32) -> i32;
}

enum CustomEvent {
    Input,
    Ipc(Value),
    Tick,
}

pub fn run(path: PathBuf) -> Result<()> {
    let document = std::cell::RefCell::new(Document::open(&path)?);
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

    let event_loop = EventLoopBuilder::<CustomEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let (input, input_receiver) = mpsc::sync_channel::<Value>(128);
    let stdin_proxy = proxy.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line
                .map_err(anyhow::Error::from)
                .and_then(|line| Ok(serde_json::from_str::<Value>(&line)?))
            {
                Ok(message) => {
                    if input.send(message).is_ok() {
                        let _ = stdin_proxy.send_event(CustomEvent::Input);
                    }
                }
                Err(error) => eprintln!("Invalid embedded webview input: {error}"),
            }
        }
    });

    // Background timer to trigger frames
    let timer_proxy = proxy.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(60));
            if timer_proxy.send_event(CustomEvent::Tick).is_err() {
                break;
            }
        }
    });

    let window = WindowBuilder::new()
        .with_title("diagrams.net")
        .with_visible(false)
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(1000.0, 700.0))
        .build(&event_loop)?;

    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler(move |request| {
            match serde_json::from_str::<Value>(request.body()) {
                Ok(message) if message.get("token").and_then(Value::as_str) == Some(&token) => {
                    let _ = ipc_proxy.send_event(CustomEvent::Ipc(message));
                }
                Ok(_) => eprintln!("Rejected embedded webview token"),
                Err(error) => eprintln!("Invalid embedded webview IPC: {error}"),
            }
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .build(&window)?;

    let active = Cell::new(true);
    let mut current_width = 1000u32;
    let mut current_height = 700u32;
    let mut last_frame = None::<Vec<u8>>;
    let hwnd = window.hwnd() as *mut c_void;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(CustomEvent::Input) => {
                while let Ok(message) = input_receiver.try_recv() {
                    let result = match message["action"].as_str() {
                        Some("resize") => {
                            let width = message["width"].as_u64().unwrap_or(1000).clamp(320, 4096) as u32;
                            let height = message["height"].as_u64().unwrap_or(700).clamp(200, 4096) as u32;
                            current_width = width;
                            current_height = height;
                            window.set_inner_size(LogicalSize::new(width as f64, height as f64));
                            Ok(())
                        }
                        Some("active") => {
                            active.set(message["active"].as_bool().unwrap_or(false));
                            Ok(())
                        }
                        Some("save") => evaluate(&webview, json!({"action": "requestSave"})),
                        Some("reload") => {
                            let path = document.borrow().path().to_path_buf();
                            match Document::open(&path) {
                                Ok(reloaded) => {
                                    *document.borrow_mut() = reloaded;
                                    emit(&output, json!({"event": "modified", "modified": false}));
                                    evaluate(
                                        &webview,
                                        json!({
                                            "action": "load",
                                            "xml": document.borrow().xml(),
                                            "autosave": 1,
                                            "noExitBtn": true,
                                            "saveAndExit": false
                                        }),
                                    )
                                }
                                Err(error) => Err(error),
                            }
                        }
                        Some("close") => {
                            *control_flow = ControlFlow::Exit;
                            Ok(())
                        }
                        Some("pointer") | Some("key") | Some("scroll") => {
                            dispatch_windows_input(hwnd, &message)
                        }
                        _ => Ok(()),
                    };

                    if let Err(error) = result {
                        emit(&output, json!({"event": "error", "message": error.to_string()}));
                    }
                }
            }
            Event::UserEvent(CustomEvent::Ipc(message)) => {
                let response = match message["event"].as_str() {
                    Some("init") => Some(json!({
                        "action": "load",
                        "xml": document.borrow().xml(),
                        "autosave": 1,
                        "noExitBtn": true,
                        "saveAndExit": false
                    })),
                    Some("save") => {
                        let result = message["xml"]
                            .as_str()
                            .context("Save has no XML")
                            .and_then(|xml| document.borrow_mut().save(xml));
                        match result {
                            Ok(()) => Some(json!({"action": "saved"})),
                            Err(error) => {
                                emit(
                                    &output,
                                    json!({"event": "saveError", "message": error.to_string()}),
                                );
                                Some(json!({"action": "saveError", "message": error.to_string()}))
                            }
                        }
                    }
                    Some("modified") => {
                        emit(&output, json!({"event": "modified", "modified": message["modified"]}));
                        None
                    }
                    Some("saved") => {
                        emit(&output, json!({"event": "saved", "modified": message["modified"]}));
                        None
                    }
                    Some("exit") => {
                        emit(&output, json!({"event": "exit"}));
                        None
                    }
                    _ => None,
                };
                if let Some(response) = response
                    && let Err(error) = evaluate(&webview, response)
                {
                    emit(&output, json!({"event": "error", "message": error.to_string()}));
                }
            }
            Event::UserEvent(CustomEvent::Tick) => {
                if active.get() {
                    if let Ok(bytes) = capture_window(hwnd, current_width, current_height) {
                        if last_frame.as_ref() != Some(&bytes) {
                            if output
                                .try_send(json!({"event": "frame", "png": STANDARD.encode(&bytes)}))
                                .is_ok()
                            {
                                last_frame = Some(bytes);
                            }
                        }
                    }
                }
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}

fn evaluate(webview: &wry::WebView, message: Value) -> Result<()> {
    webview.evaluate_script(&format!("window.receiveNative({message})"))?;
    Ok(())
}

fn emit(output: &mpsc::SyncSender<Value>, message: Value) {
    let _ = output.send(message);
}

fn dispatch_windows_input(hwnd: *mut c_void, message: &Value) -> Result<()> {
    let action = message["action"].as_str().unwrap_or("");
    match action {
        "pointer" => {
            let x = message["x"].as_f64().unwrap_or(0.0).round() as i32;
            let y = message["y"].as_f64().unwrap_or(0.0).round() as i32;
            let lparam = ((y as u16 as u32) << 16) | (x as u16 as u32);
            let pressed = message["pressed"].as_bool().unwrap_or(false);
            let button = message["button"].as_u64().unwrap_or(1);
            let kind = message["kind"].as_str().unwrap_or("");
            let msg = if kind == "move" {
                WM_MOUSEMOVE
            } else if pressed {
                match button {
                    1 => WM_LBUTTONDOWN,
                    2 => WM_MBUTTONDOWN,
                    3 => WM_RBUTTONDOWN,
                    _ => WM_LBUTTONDOWN,
                }
            } else {
                match button {
                    1 => WM_LBUTTONUP,
                    2 => WM_MBUTTONUP,
                    3 => WM_RBUTTONUP,
                    _ => WM_LBUTTONUP,
                }
            };
            unsafe {
                PostMessageW(hwnd, msg, 0, lparam as _);
            }
        }
        "scroll" => {
            let x = message["x"].as_f64().unwrap_or(0.0).round() as i32;
            let y = message["y"].as_f64().unwrap_or(0.0).round() as i32;
            let lparam = ((y as u16 as u32) << 16) | (x as u16 as u32);
            let delta_y = (message["delta_y"].as_f64().unwrap_or(0.0) * 120.0).round() as i16;
            let wparam = (delta_y as u16 as u32) << 16;
            unsafe {
                PostMessageW(hwnd, WM_MOUSEWHEEL, wparam as _, lparam as _);
            }
        }
        _ => {}
    }
    Ok(())
}

fn capture_window(hwnd: *mut c_void, width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        bail!("Invalid window dimensions");
    }

    unsafe {
        let hdc_screen = GetDC(hwnd);
        if hdc_screen.is_null() {
            bail!("Could not get window DC");
        }
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_null() {
            ReleaseDC(hwnd, hdc_screen);
            bail!("Could not create compatible DC");
        }
        let hbm = CreateCompatibleBitmap(hdc_screen, width as i32, height as i32);
        if hbm.is_null() {
            DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_screen);
            bail!("Could not create compatible bitmap");
        }
        let old_bm = SelectObject(hdc_mem, hbm);

        // PW_RENDERFULLCONTENT = 2
        PrintWindow(hwnd, hdc_mem, 2);

        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = width as i32;
        bmi.bmiHeader.biHeight = -(height as i32); // Negative for top-down DIB
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut buffer = vec![0u8; (width * height * 4) as usize];
        GetDIBits(
            hdc_mem,
            hbm,
            0,
            height,
            buffer.as_mut_ptr() as _,
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, old_bm);
        DeleteObject(hbm);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_screen);

        // Convert BGRA to RGBA
        for chunk in buffer.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }

        let mut png_bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png_bytes).write_image(
            &buffer,
            width,
            height,
            image::ExtendedColorType::Rgba8,
        )?;

        Ok(png_bytes)
    }
}
