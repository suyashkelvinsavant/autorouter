//! AutoRouter desktop binary entry point.
//!
//! Wraps the headless gateway in a Tauri shell so the same crate
//! produces a runnable window with a tray icon, native menus, and a
//! built-in dashboard.

#![deny(unused_crate_dependencies)]
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use autorouter_config as _;
use autorouter_core as _;
use autorouter_observability as _;
use autorouter_router as _;
use autorouter_server as _;
use autorouter_translate as _;
use axum as _;
use chrono as _;
use parking_lot as _;
use serde as _;
use serde_json as _;
use tauri as _;
use tauri_plugin_global_shortcut as _;
use tauri_plugin_opener as _;
use tauri_plugin_shell as _;
use tracing as _;

fn main() {
    autorouter_desktop_lib::run();
}
