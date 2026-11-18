mod api;
mod db;
mod demo;
mod engine;
mod recurrence;
#[cfg(test)]
mod tests;

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Assets;

fn asset_response(path: &str, if_none_match: Option<&str>) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let etag: String = format!(
        "\"{}\"",
        file.metadata.sha256_hash()[..12].iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    // Always revalidate (cheap 304s) so a new release shows up immediately.
    let builder = Response::builder()
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::ETAG, &etag)
        .header("X-Content-Type-Options", "nosniff");
    if if_none_match == Some(etag.as_str()) {
        return Some(builder.status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("valid response"));
    }
    Some(
        builder
            .header(header::CONTENT_TYPE, mime.as_ref())
            .body(Body::from(file.data.into_owned()))
            .expect("valid response"),
    )
}

async fn static_handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "Not found" }))).into_response();
    }
    let inm = headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok());
    if !path.is_empty()
        && let Some(r) = asset_response(path, inm)
    {
        return r;
    }
    // SPA fallback: client-side routes (/request, /status/..., /admin/...).
    asset_response("index.html", inm).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

pub fn app(state: api::AppState) -> axum::Router {
    api::router(state).fallback(static_handler)
}

const DEFAULT_DB: &str = "roomlog.db";
/// Database file used before the project was renamed from Quippy.
const LEGACY_DB: &str = "quippy.db";

/// Keep existing installs working: if only the pre-rename database exists,
/// carry on using it rather than silently starting with an empty one.
fn default_db_path() -> String {
    let exists = |p: &str| std::path::Path::new(p).exists();
    if !exists(DEFAULT_DB) && exists(LEGACY_DB) {
        eprintln!("Using legacy database {LEGACY_DB}; rename it to {DEFAULT_DB} or set DATABASE_PATH");
        return LEGACY_DB.into();
    }
    DEFAULT_DB.into()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| default_db_path());
    let conn = db::open(&db_path).unwrap_or_else(|e| {
        eprintln!("Failed to open database {db_path}: {e}");
        std::process::exit(1);
    });

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("seed-demo") {
        match demo::seed(&conn) {
            Ok(true) => println!("Demo data loaded into {db_path}"),
            Ok(false) => println!("{db_path} already has rooms; demo data not loaded"),
            Err(e) => {
                eprintln!("Failed to seed demo data: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let password = match std::env::var("ADMIN_PASSWORD") {
        Ok(p) if p.len() >= 8 => p,
        Ok(_) => {
            eprintln!("ADMIN_PASSWORD must be at least 8 characters");
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("Set ADMIN_PASSWORD to the admin console password (at least 8 characters)");
            std::process::exit(1);
        }
    };
    let cookie_secure = matches!(std::env::var("COOKIE_SECURE").as_deref(), Ok("1" | "true" | "yes"));
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    let state = api::AppState::new(conn, password, cookie_secure);
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap_or_else(|e| {
        eprintln!("Failed to bind {bind}: {e}");
        std::process::exit(1);
    });
    tracing::info!("RoomLog listening on http://{bind} (database: {db_path})");
    axum::serve(listener, app(state)).await.expect("server error");
}
