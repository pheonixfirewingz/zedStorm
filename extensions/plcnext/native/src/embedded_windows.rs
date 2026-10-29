use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageEncoder as _;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    ffi::c_void,
    io::{BufRead, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    time::Duration,
};
use tao::{
    dpi::{LogicalSize, PhysicalPosition},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::windows::WindowExtWindows,
    window::WindowBuilder,
};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Storage::Xps::PrintWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use wry::WebViewBuilder;

use zed_plcnext_viewer::Document;

const OFFSCREEN_POSITION: i32 = -32_000;

enum CustomEvent {
    Input,
    Ipc(Value),
    Quit,
}

pub fn run(path: PathBuf) -> Result<()> {
    let document = std::cell::RefCell::new(Document::open(&path)?);
    let _token = uuid::Uuid::new_v4().to_string();

    let css = include_str!("web/viewer.css");
    let js = include_str!("web/viewer.js");
    let html = include_str!("web/viewer.html")
        .replace("/* CSS_PLACEHOLDER */", css)
        .replace("/* JS_PLACEHOLDER */", js);

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
        let _ = stdin_proxy.send_event(CustomEvent::Quit);
    });

    let window = WindowBuilder::new()
        .with_title("PLCnext Engineer Viewer")
        .with_visible(false)
        .with_decorations(false)
        .with_inner_size(LogicalSize::new(1100.0, 750.0))
        .with_position(PhysicalPosition::new(OFFSCREEN_POSITION, OFFSCREEN_POSITION))
        .with_focused(false)
        .build(&event_loop)?;

    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler(move |request| {
            if let Ok(message) = serde_json::from_str::<Value>(request.body()) {
                let _ = ipc_proxy.send_event(CustomEvent::Ipc(message));
            }
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .build(&window)?;

    if let Err(error) = window.set_skip_taskbar(true) {
        eprintln!("Could not hide from taskbar: {error}");
    }
    if !reveal_for_capture(window.hwnd() as *mut c_void) {
        eprintln!("Window could not be shown for offscreen capture");
    }

    let active = Arc::new(AtomicBool::new(true));
    let frame_width = Arc::new(AtomicU32::new(1100));
    let frame_height = Arc::new(AtomicU32::new(750));
    let hwnd = window.hwnd() as *mut c_void;
    let input_target = Cell::new(hwnd);

    // Frame capture thread
    std::thread::spawn({
        let output = output.clone();
        let active = active.clone();
        let frame_width = frame_width.clone();
        let frame_height = frame_height.clone();
        let capture_hwnd = hwnd as usize;
        move || {
            let mut last_frame = None::<Vec<u8>>;
            let mut reported_failure = false;
            loop {
                std::thread::sleep(Duration::from_millis(50));
                if !active.load(Ordering::Relaxed) {
                    continue;
                }
                let width = frame_width.load(Ordering::Relaxed);
                let height = frame_height.load(Ordering::Relaxed);
                let bytes = match capture_window(capture_hwnd as *mut c_void, width, height) {
                    Ok(bytes) => {
                        reported_failure = false;
                        bytes
                    }
                    Err(error) => {
                        if !reported_failure {
                            reported_failure = true;
                            eprintln!("Could not capture embedded webview: {error}");
                        }
                        continue;
                    }
                };
                if last_frame.as_ref() == Some(&bytes) {
                    continue;
                }
                if output
                    .try_send(json!({"event": "frame", "png": STANDARD.encode(&bytes)}))
                    .is_ok()
                {
                    last_frame = Some(bytes);
                }
            }
        }
    });

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(CustomEvent::Quit) => {
                *control_flow = ControlFlow::ExitWithCode(0);
            }
            Event::UserEvent(CustomEvent::Input) => {
                while let Ok(message) = input_receiver.try_recv() {
                    let result = match message["action"].as_str() {
                        Some("resize") => {
                            let width = message["width"].as_u64().unwrap_or(1100).clamp(320, 4096) as u32;
                            let height = message["height"].as_u64().unwrap_or(750).clamp(200, 4096) as u32;
                            frame_width.store(width, Ordering::Relaxed);
                            frame_height.store(height, Ordering::Relaxed);
                            window.set_inner_size(LogicalSize::new(width as f64, height as f64));
                            Ok(())
                        }
                        Some("active") => {
                            active.store(message["active"].as_bool().unwrap_or(false), Ordering::Relaxed);
                            Ok(())
                        }
                        Some("save") => {
                            // Read-only viewer: save is always clean
                            emit(&output, json!({"event": "saved", "modified": false}));
                            Ok(())
                        }
                        Some("reload") => {
                            let path = document.borrow().path().to_path_buf();
                            match Document::open(&path) {
                                Ok(reloaded) => {
                                    *document.borrow_mut() = reloaded;
                                    let project_json = document.borrow().to_json();
                                    let script = format!("window.loadProject({})", project_json);
                                    let _ = webview.evaluate_script(&script);
                                    emit(&output, json!({"event": "modified", "modified": false}));
                                    Ok(())
                                }
                                Err(e) => Err(e),
                            }
                        }
                        Some("close") => {
                            *control_flow = ControlFlow::Exit;
                            Ok(())
                        }
                        Some("pointer") | Some("key") | Some("scroll") => {
                            dispatch_windows_input(hwnd, &input_target, &message)
                        }
                        _ => Ok(()),
                    };

                    if let Err(error) = result {
                        emit(&output, json!({"event": "error", "message": error.to_string()}));
                    }
                }
            }
            Event::UserEvent(CustomEvent::Ipc(message)) => {
                if message["event"].as_str() == Some("init") {
                    let project_json = document.borrow().to_json();
                    let script = format!("window.loadProject({})", project_json);
                    if let Err(error) = webview.evaluate_script(&script) {
                        eprintln!("Failed to load project into webview: {error}");
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

fn reveal_for_capture(hwnd: *mut c_void) -> bool {
    unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            OFFSCREEN_POSITION,
            OFFSCREEN_POSITION,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER | SWP_SHOWWINDOW,
        ) != 0
    }
}

fn emit(output: &mpsc::SyncSender<Value>, message: Value) {
    let _ = output.send(message);
}

fn dispatch_windows_input(
    hwnd: *mut c_void,
    input_target: &Cell<*mut c_void>,
    message: &Value,
) -> Result<()> {
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
            let target = webview_window(hwnd, x, y);
            input_target.set(target);
            unsafe {
                PostMessageW(target, msg, 0, lparam as _);
            }
        }
        "scroll" => {
            let x = message["x"].as_f64().unwrap_or(0.0).round() as i32;
            let y = message["y"].as_f64().unwrap_or(0.0).round() as i32;
            let screen_x = OFFSCREEN_POSITION + x;
            let screen_y = OFFSCREEN_POSITION + y;
            let lparam = ((screen_y as u16 as u32) << 16) | (screen_x as u16 as u32);
            let delta_y = (message["delta_y"].as_f64().unwrap_or(0.0) * 120.0).round() as i16;
            let wparam = (delta_y as u16 as u32) << 16;
            unsafe {
                PostMessageW(
                    webview_window(hwnd, x, y),
                    WM_MOUSEWHEEL,
                    wparam as _,
                    lparam as _,
                );
            }
        }
        "key" => dispatch_key(input_target.get(), message),
        _ => {}
    }
    Ok(())
}

fn webview_window(hwnd: *mut c_void, x: i32, y: i32) -> *mut c_void {
    let mut target = hwnd;
    loop {
        let child = unsafe { RealChildWindowFromPoint(target, POINT { x, y }) };
        if child.is_null() || child == target {
            return target;
        }
        target = child;
    }
}

fn dispatch_key(target: *mut c_void, message: &Value) {
    let pressed = message["pressed"].as_bool().unwrap_or(false);
    let key = message["key"].as_str().unwrap_or("");
    if key.is_empty() || target.is_null() {
        return;
    }
    if let Some(virtual_key) = virtual_key(key) {
        let key_up: usize = if pressed { 0 } else { (1 << 30) | (1 << 31) };
        let lparam: usize = 1 | key_up;
        let msg = if pressed { WM_KEYDOWN } else { WM_KEYUP };
        unsafe {
            PostMessageW(target, msg, usize::from(virtual_key), lparam as _);
        }
        return;
    }
    let mut characters = key.chars();
    if pressed
        && let (Some(character), None) = (characters.next(), characters.next())
    {
        unsafe {
            PostMessageW(target, WM_CHAR, character as usize, 0);
        }
    }
}

fn virtual_key(key: &str) -> Option<u16> {
    Some(match key {
        "enter" => 0x0D,
        "tab" => 0x09,
        "backspace" => 0x08,
        "escape" => 0x1B,
        "space" => 0x20,
        "delete" => 0x2E,
        "insert" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        _ => {
            let number = key.strip_prefix('f')?.parse::<u16>().ok()?;
            if !(1..=12).contains(&number) {
                return None;
            }
            0x70 + number - 1
        }
    })
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
        if PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT) == 0 {
            SelectObject(hdc_mem, old_bm);
            DeleteObject(hbm);
            DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_screen);
            bail!("Could not capture embedded webview window");
        }

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
