use axum::{
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../app/dist/"]
struct Assets;

pub async fn handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    // API paths must 404 as APIs, never fall back to the SPA shell:
    // a 200 text/html response to a JSON client hides typos and
    // removed endpoints (observed live with a deleted route — the
    // caller saw index.html instead of an error).
    if path.starts_with("api/") {
        return not_found();
    }
    serve_asset(if path.is_empty() { "index.html" } else { path })
        .unwrap_or_else(|| serve_asset("index.html").unwrap_or_else(not_found))
}

fn serve_asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Vite emits content-hashed filenames under assets/, safe to
    // cache forever; everything else (index.html, favicon) keeps a
    // stable name and must be revalidated so deploys take effect.
    let cache_control = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Some(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime.as_ref())
            .header(header::CACHE_CONTROL, cache_control)
            .body(Body::from(file.data))
            .unwrap_or_else(|_| not_found()),
    )
}

fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}
