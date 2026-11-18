mod api;
mod db;
mod engine;
mod recurrence;

pub fn app(state: api::AppState) -> axum::Router {
    api::router(state)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| "quippy.db".into());
    let conn = db::open(&db_path).unwrap_or_else(|e| {
        eprintln!("Failed to open database {db_path}: {e}");
        std::process::exit(1);
    });

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
    tracing::info!("Quippy listening on http://{bind} (database: {db_path})");
    axum::serve(listener, app(state)).await.expect("server error");
}
