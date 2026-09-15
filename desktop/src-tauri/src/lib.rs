//! OmniRoute desktop shell.
//!
//! Tauri v2 replaces the Electron shell. The window hosts the Svelte UI from
//! `../ui`, which talks to the Rust gateway over HTTP.

/// Launch the desktop application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
