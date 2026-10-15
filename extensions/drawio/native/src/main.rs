use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::WebViewBuilder;
use zed_diagrams::Document;
#[cfg(target_os = "linux")]
mod embedded;
#[cfg(target_os = "windows")]
mod embedded_windows;

#[derive(Deserialize)]
struct EditorEvent {
    token: String,
    event: String,
    xml: Option<String>,
    #[serde(default)]
    exit: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("diagrams.net: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments
        .next()
        .context("Usage: zed-diagrams [--root PROJECT] FILE.drawio")?;
    if first == "--embedded" {
        #[cfg(target_os = "linux")]
        return embedded::run(PathBuf::from(
            arguments.next().context("Missing .drawio path")?,
        ));
        #[cfg(target_os = "windows")]
        return embedded_windows::run(PathBuf::from(
            arguments.next().context("Missing .drawio path")?,
        ));
        #[cfg(not(any(target_os = "linux", target_os = "windows")))]
        bail!("Embedded system webviews are currently supported on Linux and Windows");
    }
    if first == "--help" {
        println!("Usage: zed-diagrams [--root PROJECT] FILE.drawio");
        return Ok(());
    }
    if first == "--probe-webview" {
        println!("{}", wry::webview_version()?);
        return Ok(());
    }
    let path = if first == "--root" {
        let root = PathBuf::from(arguments.next().context("Missing project root")?);
        root.join(arguments.next().context("Missing .drawio path")?)
    } else {
        PathBuf::from(first)
    };
    if arguments.next().is_some() {
        bail!("Expected one .drawio path; quote paths containing spaces");
    }
    let mut document = Document::open(&path)?;
    let token = uuid::Uuid::new_v4().to_string();
    let html = include_str!("editor.html")
        .replace("/* BRIDGE */", include_str!("bridge.js"))
        .replace("__BRIDGE_TOKEN__", &token);
    let event_loop = EventLoopBuilder::<EditorEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(format!("{} — diagrams.net", document.path().display()))
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(LogicalSize::new(640.0, 480.0))
        .build(&event_loop)?;
    let proxy = event_loop.create_proxy();
    let builder = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler(move |request| {
            match serde_json::from_str::<EditorEvent>(request.body()) {
                Ok(event) if event.token == token => {
                    if let Err(error) = proxy.send_event(event) {
                        eprintln!("Could not deliver editor event: {error}");
                    }
                }
                Ok(_) => eprintln!("Rejected an editor event with an invalid token"),
                Err(error) => eprintln!("Invalid editor event: {error}"),
            }
        })
        .with_navigation_handler(|url| {
            url == "about:blank" || url.starts_with("https://embed.diagrams.net/")
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny);
    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let container = window
            .default_vbox()
            .context("Native window has no GTK container")?;
        builder.build_gtk(container)?
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let action = match event {
            Event::UserEvent(event) => match event.event.as_str() {
                "init" => Some(json!({
                    "action": "load", "xml": document.xml(), "autosave": 1, "saveAndExit": true,
                    "title": document.path().file_name().map(|name| name.to_string_lossy()),
                })),
                "save" => {
                    let result = event
                        .xml
                        .as_deref()
                        .context("Save event missing XML")
                        .and_then(|xml| document.save(xml));
                    match result {
                        Ok(()) => Some(json!({"action": "saved", "exit": event.exit})),
                        Err(error) => {
                            eprintln!("Could not save diagram: {error:#}");
                            Some(json!({"action": "saveError", "message": format!("{error:#}")}))
                        }
                    }
                }
                "exit" => {
                    *control_flow = ControlFlow::Exit;
                    None
                }
                _ => None,
            },
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => Some(json!({"action": "close"})),
            _ => None,
        };
        if let Some(action) = action {
            let script = format!("window.receiveNative({action})");
            if let Err(error) = webview.evaluate_script(&script) {
                eprintln!("Could not update the diagram editor: {error}");
                window.set_title("diagrams.net — editor communication failed; reopen the window");
            }
        }
    });
}

