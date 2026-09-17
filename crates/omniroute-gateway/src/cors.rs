//! Minimal CORS for the dashboard's other homes.
//!
//! Same-origin browsers never send `Origin`, and the Tauri shell as well as
//! `npm run dev` do — from loopback or the Tauri custom scheme. This answers
//! preflights and echoes back exactly those origins, without pulling in
//! `tower-http`. Calls still need a bearer key, so an allowlisted origin
//! alone grants nothing.

use axum::{
    body::Body,
    http::{Method, StatusCode, header},
    middleware::Next,
    response::Response,
};

/// Origins allowed to call the API cross-origin: Tauri shells and loopback
/// dev servers (any port). Hosts must match exactly — `localhost.evil.com`
/// is not localhost.
pub fn is_allowed_origin(origin: &str) -> bool {
    if matches!(
        origin,
        "tauri://localhost" | "https://tauri.localhost" | "http://tauri.localhost"
    ) {
        return true;
    }
    let Some(("http", rest) | ("https", rest)) = origin.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or("");
    // Strip the port, respecting `[::1]` brackets.
    let host = if let Some(inner) = authority.strip_prefix('[') {
        match inner.split_once(']') {
            Some((ip, _)) => format!("[{ip}]"),
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };
    matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

fn cors_headers(headers: &mut axum::http::HeaderMap, origin: &str) {
    if let Ok(value) = header::HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(
        header::VARY,
        header::HeaderValue::from_static("Origin"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("GET, POST, PATCH, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static("authorization, content-type"),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        header::HeaderValue::from_static("86400"),
    );
}

pub async fn cors(
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let allowed = is_allowed_origin(&origin);

    if req.method() == Method::OPTIONS {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NO_CONTENT;
        if allowed {
            cors_headers(response.headers_mut(), &origin);
        }
        return response;
    }

    let mut response = next.run(req).await;
    if allowed {
        cors_headers(response.headers_mut(), &origin);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_covers_shells_and_loopback() {
        for origin in [
            "tauri://localhost",
            "https://tauri.localhost",
            "http://tauri.localhost",
            "http://localhost:5173",
            "https://127.0.0.1:9999",
            "http://[::1]:3000",
        ] {
            assert!(is_allowed_origin(origin), "{origin}");
        }
    }

    #[test]
    fn lookalikes_and_remote_hosts_are_rejected() {
        for origin in [
            "",
            "null",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "http://evil.com",
            "https://tauri.localhost.evil.com",
            "ftp://localhost",
            "http://[::2]",
        ] {
            assert!(!is_allowed_origin(origin), "{origin}");
        }
    }
}
