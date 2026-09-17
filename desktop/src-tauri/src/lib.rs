//! OmniRoute desktop shell.
//!
//! Tauri v2 replaces the Electron shell. The window hosts the Svelte UI from
//! `../ui` — the same bundle the gateway serves itself, so the desktop
//! interface is identical to the web one.
//!
//! Production is self-contained: the shell spawns its own gateway sidecar
//! (bundled `omniroute-gateway`, loopback-only on the gateway default port)
//! and injects its URL before any bundle code runs. The plugin kills the
//! sidecar with the app. In dev the Vite proxy already points at a gateway,
//! so no sidecar is spawned.

use tauri::{WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_shell::ShellExt;
use tauri_plugin_shell::process::CommandEvent;

/// Sidecar port: the gateway's own default. The launchd service overrides to
/// 20128, so the two never collide. Overridable via `OMNIROUTE_SIDECAR_PORT`.
const SIDECAR_PORT: &str = "20129";

fn sidecar_port() -> String {
    std::env::var("OMNIROUTE_SIDECAR_PORT")
        .ok()
        .filter(|port| !port.trim().is_empty())
        .unwrap_or_else(|| SIDECAR_PORT.to_string())
}

/// Launch the desktop application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .build(),
            )?;

            let mut init_script = String::new();
            if !cfg!(debug_assertions) {
                let port = sidecar_port();
                let gateway_url = format!("http://127.0.0.1:{port}");
                let (mut events, child) = app
                    .shell()
                    .sidecar("omniroute-gateway")?
                    .env("OMNIROUTE_RUST_PORT", &port)
                    .spawn()?;
                // The child is kept alive by the task; the plugin kills it
                // with the app. Its output streams into the log.
                tauri::async_runtime::spawn(async move {
                    let _keep = child;
                    while let Some(event) = events.recv().await {
                        match event {
                            CommandEvent::Stdout(line) => {
                                log::info!(target: "gateway", "{}", String::from_utf8_lossy(&line));
                            }
                            CommandEvent::Stderr(line) => {
                                log::warn!(target: "gateway", "{}", String::from_utf8_lossy(&line));
                            }
                            CommandEvent::Terminated(payload) => {
                                log::warn!(target: "gateway", "sidecar exited: {payload:?}");
                            }
                            _ => {}
                        }
                    }
                });
                init_script = format!("window.__OMNIROUTE_GATEWAY_URL__={gateway_url:?};");
            }

            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("OmniRoute")
                .inner_size(1280.0, 860.0)
                .resizable(true)
                .initialization_script(&init_script)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
