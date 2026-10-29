use std::path::PathBuf;
use anyhow::{Context, Result};
use serde_json::Value;
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::WebViewBuilder;
use zed_plcnext_viewer::Document;

#[cfg(target_os = "windows")]
mod embedded_windows;
#[cfg(not(target_os = "windows"))]
mod embedded;

fn main() {
    if let Err(error) = run() {
        eprintln!("zed-plcnext-viewer: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        println!("PLCnext Engineer Project Viewer");
        println!("Usage:");
        println!("  zed-plcnext-viewer [--embedded] <path-to-project.pcwex|.pcweax|.pcwlx|.pcwef>");
        println!("  zed-plcnext-viewer --info <project-path>");
        println!("  zed-plcnext-viewer --export-st <project-path> [output.st]");
        println!("  zed-plcnext-viewer --export-csv <project-path> [output.csv]");
        return Ok(());
    }

    if args[0] == "--embedded" {
        let path = PathBuf::from(args.get(1).context("Missing project path for --embedded")?);
        #[cfg(target_os = "windows")]
        return embedded_windows::run(path);
        #[cfg(not(target_os = "windows"))]
        return embedded::run(path);
    }

    if args[0] == "--info" {
        let path = PathBuf::from(args.get(1).context("Missing project path for --info")?);
        let doc = Document::open(&path)?;
        println!("{}", serde_json::to_string_pretty(&doc.to_json())?);
        return Ok(());
    }

    if args[0] == "--export-st" {
        let path = PathBuf::from(args.get(1).context("Missing project path for --export-st")?);
        let doc = Document::open(&path)?;
        let st_code = doc.export_all_st();
        if let Some(out_path) = args.get(2) {
            std::fs::write(out_path, &st_code)?;
            println!("Exported Structured Text to {out_path}");
        } else {
            print!("{st_code}");
        }
        return Ok(());
    }

    if args[0] == "--export-csv" {
        let path = PathBuf::from(args.get(1).context("Missing project path for --export-csv")?);
        let doc = Document::open(&path)?;
        let csv = doc.export_variables_csv();
        if let Some(out_path) = args.get(2) {
            std::fs::write(out_path, &csv)?;
            println!("Exported variables to {out_path}");
        } else {
            print!("{csv}");
        }
        return Ok(());
    }

    // Standalone Interactive GUI Window
    let path = PathBuf::from(&args[0]);
    let doc = Document::open(&path)?;

    let css = include_str!("web/viewer.css");
    let js = include_str!("web/viewer.js");
    let html = include_str!("web/viewer.html")
        .replace("/* CSS_PLACEHOLDER */", css)
        .replace("/* JS_PLACEHOLDER */", js);

    let event_loop = EventLoopBuilder::<Value>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title(format!("{} \u{2014} PLCnext Engineer Project Viewer (Read Only)", doc.project().project_name))
        .with_inner_size(LogicalSize::new(1200.0, 800.0))
        .with_min_inner_size(LogicalSize::new(800.0, 500.0))
        .build(&event_loop)?;

    let proxy = event_loop.create_proxy();
    let webview = WebViewBuilder::new()
        .with_html(html)
        .with_ipc_handler(move |request| {
            if let Ok(val) = serde_json::from_str::<Value>(request.body()) {
                let _ = proxy.send_event(val);
            }
        })
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .build(&window)?;

    let project_json = doc.to_json();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(val) => {
                if val["event"].as_str() == Some("init") {
                    let script = format!("window.loadProject({})", project_json);
                    let _ = webview.evaluate_script(&script);
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
