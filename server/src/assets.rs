use axum::{
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; connect-src 'self' ws: wss:; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; script-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";

#[derive(RustEmbed)]
#[folder = "frontend/dist/"]
pub struct Assets;

pub async fn serve_root() -> Response {
    serve("index.html")
}
pub async fn serve_path(Path(path): Path<String>) -> Response {
    serve(if path.is_empty() { "index.html" } else { &path })
}

fn serve(path: &str) -> Response {
    if path == "api" || path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, "API route not found").into_response();
    }
    let (asset_path, candidate) = match Assets::get(path) {
        Some(asset) => (path, Some(asset)),
        None => ("index.html", Assets::get("index.html")),
    };
    match candidate {
        Some(content) => {
            let mime = mime_guess::from_path(asset_path).first_or_octet_stream();
            let mut response = (
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (
                        header::CACHE_CONTROL,
                        if asset_path == "index.html" {
                            "no-cache"
                        } else {
                            "public, max-age=31536000, immutable"
                        },
                    ),
                ],
                content.data,
            )
                .into_response();
            add_security_headers(&mut response);
            response
        }
        None => (StatusCode::NOT_FOUND, "frontend not built").into_response(),
    }
}

fn add_security_headers(response: &mut Response) {
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().expect("static header"),
    );
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        "no-referrer".parse().expect("static header"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        CONTENT_SECURITY_POLICY.parse().expect("static header"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_security_policy_allows_local_image_previews() {
        assert!(CONTENT_SECURITY_POLICY.contains("img-src 'self' data: blob:"));
    }
}
