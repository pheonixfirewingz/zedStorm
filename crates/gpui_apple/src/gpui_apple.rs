#![cfg(target_os = "macos")]
//! Shared Apple rendering and task dispatch for GPUI.
//!
//! This crate renders GPUI scenes directly with Metal on Apple platforms. It
//! owns GPU resources and shaders while leaving application lifecycle,
//! windowing, and input to each platform backend.

mod dispatcher;
mod metal_atlas;
pub mod metal_renderer;

pub use dispatcher::{AppleActivity, AppleDispatcher};
