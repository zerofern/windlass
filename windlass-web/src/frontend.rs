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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the SPA fallback used to serve index.html (200,
    /// text/html) for unmatched `/api/*` paths, hiding typos and
    /// removed endpoints from JSON clients.  API paths must 404.
    #[tokio::test]
    async fn unmatched_api_paths_get_404_not_spa_html() {
        let res = handler("/api/v1/does-not-exist".parse().unwrap()).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    /// The SPA shell serves for `/` and unknown UI routes (deep links),
    /// and must carry `no-cache` so deploys take effect immediately.
    #[tokio::test]
    async fn spa_shell_serves_with_no_cache() {
        for path in ["/", "/queue", "/some/deep/link"] {
            let res = handler(path.parse().unwrap()).await;
            assert_eq!(res.status(), StatusCode::OK, "path {path}");
            assert_eq!(
                res.headers().get(header::CONTENT_TYPE).unwrap(),
                "text/html",
                "path {path}"
            );
            assert_eq!(
                res.headers().get(header::CACHE_CONTROL).unwrap(),
                "no-cache",
                "path {path}"
            );
        }
    }

    /// Vite's content-hashed bundles are immutable; everything under
    /// `assets/` gets the year-long immutable cache policy.
    #[tokio::test]
    async fn hashed_assets_are_cached_immutably() {
        // Find a real asset name from the embedded index.html so the
        // test tracks whatever hash the current build produced.
        let index = Assets::get("index.html").expect("embedded index.html");
        let html = String::from_utf8_lossy(&index.data).into_owned();
        let start = html.find("assets/").expect("index references an asset");
        let end = html[start..]
            .find('"')
            .map(|i| start + i)
            .expect("attribute closes");
        let asset = &html[start..end];

        let res = handler(format!("/{asset}").parse().unwrap()).await;
        assert_eq!(res.status(), StatusCode::OK, "asset {asset}");
        assert_eq!(
            res.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=31536000, immutable",
            "asset {asset}"
        );
    }
}
