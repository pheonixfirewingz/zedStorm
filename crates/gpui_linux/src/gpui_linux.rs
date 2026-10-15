#![cfg(target_os = "linux")]
mod linux;

pub use linux::{current_platform, linux_platform};
