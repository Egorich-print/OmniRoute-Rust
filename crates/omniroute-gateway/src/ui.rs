//! Static file serving for the dashboard SPA.
//!
//! The UI is a Vite/Svelte bundle in `desktop/ui/dist`, served from the same
//! origin as the API so the "main page" keeps living at the gateway's own
//! address (`http://localhost:20128/`) exactly like the JS dashboard did.
//!
//! Set `OMNIROUTE_UI_DIR` to point at another build. When the directory is
//! missing the gateway stays API-only and `/` explains how to build the UI,
//! rather than 404ing silently.

use std::path::{Path, PathBuf};

use axum::{
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

use crate::AppState;

/// Resolve the UI directory: explicit env var, else the build in this repo.
pub fn ui_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("OMNIROUTE_UI_DIR")
        && !dir.trim().is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    let default = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../desktop/ui/dist");
    default.is_dir().then_some(default)
}

/// Serve a path from the UI bundle, falling back to `index.html`.
pub async fn serve(State(state): State<AppState>, uri: Uri) -> Response {
    let Some(dir) = state.ui_dir.as_ref() else {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "dashboard not built: run `npm install && npm run build` in desktop/ui, \
             or set OMNIROUTE_UI_DIR",
        )
            .into_response();
    };

    let relative = uri.path().trim_start_matches('/');
    let index = dir.join("index.html");

    let file = if relative.is_empty() {
        index
    } else if let Some(path) = safe_join(dir, relative) {
        // Unknown path: hand back the SPA shell so client-side routes work.
        if path.is_file() { path } else { index }
    } else {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "invalid path",
        )
            .into_response();
    };

    if !file.is_file() {
        return StatusCode::NOT_FOUND.into_response();
    }

    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let content_type = content_type(&file);
            ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Join `relative` under `dir`, refusing traversal outside it.
fn safe_join(dir: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let mut path = dir.to_path_buf();
    for segment in relative.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains('\\') {
            return None;
        }
        path.push(segment);
    }
    Some(path)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal() {
        let dir = Path::new("/srv/ui");
        assert_eq!(safe_join(dir, "../secret"), None);
        assert_eq!(safe_join(dir, "assets/../../secret"), None);
        assert_eq!(safe_join(dir, "a/../../b"), None);
        // The handler trims the leading `/`, so the root arrives as "".
        assert_eq!(safe_join(dir, ""), None);
        assert_eq!(safe_join(dir, "../"), None);
        assert_eq!(
            safe_join(dir, "assets/app.js"),
            Some(dir.join("assets/app.js"))
        );
    }

    #[test]
    fn content_types_cover_the_bundle() {
        assert!(content_type(Path::new("index.html")).starts_with("text/html"));
        assert!(content_type(Path::new("app.js")).starts_with("text/javascript"));
        assert_eq!(content_type(Path::new("logo.svg")), "image/svg+xml");
        assert_eq!(
            content_type(Path::new("blob.bin")),
            "application/octet-stream"
        );
    }
}
