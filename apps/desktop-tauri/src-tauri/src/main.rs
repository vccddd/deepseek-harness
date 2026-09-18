//! Tauri shell prototype: application window, `dsh-app` protocol, Host
//! supervision, stream bridge, and the boot IPC consumed by the shared Web
//! client. The shell holds every credential; the page never sees one.
//!
//! Native integration uses the maintained Tauri ecosystem pieces: the dialog
//! plugin for fatal message boxes and directory picking, single-instance for
//! the profile lock, `set_theme` for palette sync, and `window-vibrancy` for
//! the macOS sidebar material.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod host;
mod paths;
mod state;
mod web_document;
mod ws_bridge;

use std::sync::{Arc, OnceLock};

use tauri::{Manager, TitleBarStyle, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;

use crate::state::ShellState;

/// Origin of the desktop application document.
pub const APP_ORIGIN: &str = "dsh-app://app";

static SHELL: OnceLock<Arc<ShellState>> = OnceLock::new();

/// Await Host readiness and answer the page boot handshake.
#[tauri::command]
async fn desktop_boot(state: tauri::State<'_, Arc<ShellState>>) -> Result<serde_json::Value, String> {
    state.wait_for_boot().await
}

/// Receive a client-side boot failure and present it natively and on the page.
#[tauri::command]
async fn desktop_boot_failed(state: tauri::State<'_, Arc<ShellState>>, message: String) -> Result<(), String> {
    eprintln!("dsh desktop tauri: client boot failed: {message}");
    state.show_fatal(&message);
    Ok(())
}

/// Open the native directory picker owned by the application window.
#[tauri::command]
async fn desktop_pick_directory(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| String::from("dsh desktop tauri: main window is unavailable"))?;
    let _ = window.show();
    let _ = window.set_focus();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_parent(&window)
            .set_title("Choose Workspace Directory")
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| format!("directory picker task failed: {error}"))?;
    Ok(picked.and_then(|path| path.into_path().ok().and_then(|path| path.into_os_string().into_string().ok())))
}

/// Mirror the Web UI theme source into the native window theme (vibrancy follows).
#[tauri::command]
async fn desktop_set_theme(app: tauri::AppHandle, source: String) -> Result<(), String> {
    match source.as_str() {
        "light" => app.set_theme(Some(tauri::Theme::Light)),
        "dark" => app.set_theme(Some(tauri::Theme::Dark)),
        "system" => app.set_theme(None),
        other => return Err(format!("dsh desktop tauri: unknown theme source {other}")),
    }
    Ok(())
}

/// The `process.platform` value the shared Web CSS branches on.
fn page_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        "linux" => "linux",
        _ => std::env::consts::OS,
    }
}

/// Script installed ahead of every page script: the desktop bridge globals.
///
/// Mirrors the Electron preload surface: `dshDesktopBoot` (boot handshake),
/// `dshDesktop` (product API stub), `__DSH_DIRECTORY_PICKER__`, the
/// `data-platform` mark, and — on macOS — the `data-ds-theme-source`
/// observer that keeps the native theme and sidebar vibrancy on the app
/// palette.
fn init_script() -> String {
    let theme_observer = if std::env::consts::OS == "macos" {
        r#"
  var sent
  var sendTheme = function () {
    var value = root.getAttribute('data-ds-theme-source')
    if (value === null || value === sent) return
    sent = value
    internals.invoke('desktop_set_theme', { source: value })
  }
  var observeTheme = function () {
    new MutationObserver(sendTheme).observe(root, { attributeFilter: ['data-ds-theme-source'] })
    sendTheme()
  }
  if (document.readyState === 'loading') window.addEventListener('DOMContentLoaded', observeTheme)
  else observeTheme()"#
    } else {
        ""
    };
    format!(
        r#";(function () {{
  'use strict'
  var root = document.documentElement
  if (root !== null) root.setAttribute('data-platform', {platform})
  var internals = globalThis.__TAURI_INTERNALS__
  globalThis.dshDesktop = {{ protocolVersion: 1 }}
  if (internals === undefined || typeof internals.invoke !== 'function') return
  globalThis.dshDesktopBoot = {{
    ready: function () {{ return internals.invoke('desktop_boot') }},
    failed: function (message) {{ return internals.invoke('desktop_boot_failed', {{ message: String(message) }}) }},
  }}
  globalThis.__DSH_DIRECTORY_PICKER__ = {{
    pick: function () {{ return internals.invoke('desktop_pick_directory') }},
  }}{theme_observer}
}})()"#,
        platform = serde_json::to_string(page_platform()).expect("platform string serializes"),
        theme_observer = theme_observer,
    )
}

/// Allow only application-document navigation and same-loopback HTTP.
fn navigation_allowed(url: &tauri::Url) -> bool {
    let scheme = url.scheme();
    if scheme == "dsh-app" {
        return true;
    }
    if scheme == "http" || scheme == "https" {
        return matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
    }
    eprintln!("dsh desktop tauri: blocked navigation to {url}");
    false
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .register_asynchronous_uri_scheme_protocol("dsh-app", |_ctx, request, responder| {
            let Some(state) = SHELL.get() else {
                responder.respond(
                    tauri::http::Response::builder()
                        .status(503)
                        .body(std::borrow::Cow::Borrowed(&[][..]))
                        .expect("static response builds"),
                );
                return;
            };
            tauri::async_runtime::spawn(async move {
                let response = web_document::handle(state, request).await;
                responder.respond(response);
            });
        })
        .invoke_handler(tauri::generate_handler![
            desktop_boot,
            desktop_boot_failed,
            desktop_pick_directory,
            desktop_set_theme
        ])
        .setup(|app| {
            let shell_paths = paths::ShellPaths::resolve()?;
            let state = Arc::new(ShellState::new(shell_paths));
            let bridge_port = ws_bridge::start(state.clone())?;
            state.set_stream_base(format!("http://127.0.0.1:{bridge_port}"));
            let url: tauri::Url = format!("{APP_ORIGIN}/").parse().map_err(|error| format!("application url: {error}"))?;
            let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::CustomProtocol(url))
                .title("DeepSeek Harness")
                .inner_size(1280.0, 840.0)
                .min_inner_size(880.0, 600.0)
                .initialization_script(init_script())
                .on_navigation(navigation_allowed);
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(TitleBarStyle::Overlay)
                // The sidebar vibrancy material shows through the page background;
                // macOS transparency needs the tauri macos-private-api feature.
                .transparent(true);
            let window = builder.build()?;
            #[cfg(target_os = "macos")]
            if let Err(error) = window_vibrancy::apply_vibrancy(
                &window,
                window_vibrancy::NSVisualEffectMaterial::Sidebar,
                Some(window_vibrancy::NSVisualEffectState::Active),
                None,
            ) {
                eprintln!("dsh desktop tauri: applying sidebar vibrancy failed: {error}");
            }
            state.set_window(window);
            state.set_app(app.handle().clone());
            let host = host::start(state.clone());
            state.set_host(host);
            SHELL.set(state.clone()).map_err(|_| "shell state installed twice")?;
            app.manage(state);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build the Tauri shell")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(host) = app.try_state::<Arc<ShellState>>().and_then(|state| state.host()) {
                    host.stop_blocking();
                }
            }
        });
}
