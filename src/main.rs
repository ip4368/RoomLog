mod api;
mod auth;
mod bookings;
mod clash;
mod db;
mod demo;
mod engine;
mod fleet;
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

async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
}

pub fn app(state: api::AppState) -> axum::Router {
    api::router(state).route("/healthz", axum::routing::get(health)).fallback(static_handler)
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

/// On first start there are no staff accounts: create an `admin` account from
/// `ADMIN_PASSWORD`. Upgrades from the shared-password version keep working,
/// since that variable was already set.
fn bootstrap_admin(conn: &rusqlite::Connection) {
    let count = auth::user_count(conn).unwrap_or_else(|e| {
        eprintln!("Failed to read staff accounts: {e}");
        std::process::exit(1);
    });
    let password = std::env::var("ADMIN_PASSWORD").ok();
    if count > 0 {
        if password.is_some() {
            tracing::warn!("ADMIN_PASSWORD is ignored now that staff accounts exist; manage them under Users");
        }
        return;
    }
    let Some(password) = password else {
        eprintln!("No staff accounts yet. Set ADMIN_PASSWORD to create the first admin account (username \"admin\").");
        std::process::exit(1);
    };
    match auth::create_user(conn, "admin", "Administrator", &password, &[auth::Role::Admin]) {
        Ok(_) => tracing::info!("Created admin account \"admin\" from ADMIN_PASSWORD. Add staff accounts under Users."),
        Err(e) => {
            eprintln!("Could not create the admin account: {e}");
            std::process::exit(1);
        }
    }
}

const USAGE: &str = "\
Usage:
  roomlog                                   serve (single church, or fleet when TENANTS_DIR is set)
  roomlog seed-demo                         load sample data into an empty single database
  roomlog reset-password <username>         reset a staff password (read from stdin)
  roomlog backup <destination.db>           snapshot the single database (DATABASE_PATH)
  roomlog check-database <file.db>          validate a database file for import/use
  roomlog tenant-create <slug>              create a fleet tenant (TENANTS_DIR, ADMIN_PASSWORD)
  roomlog tenant-list                       list fleet tenants (TENANTS_DIR)
  roomlog tenant-backup <slug> <dest.db>    snapshot one fleet tenant (TENANTS_DIR)
  roomlog tenant-import <slug> <file.db>    import a database file as a new tenant (TENANTS_DIR)
  roomlog tenant-backup-all <dest-dir>      snapshot every tenant + manifest.json (TENANTS_DIR)
  roomlog verify-backups <backup-dir>       re-validate a backup set against its manifest
  roomlog restore-drill <slug|file.db>      prove a snapshot restores (TENANTS_DIR => slug)
  roomlog tenant-versions                   schema version per tenant (TENANTS_DIR)
  roomlog backup-config <replica-url>       print a Litestream config (fleet or single database)";

fn die(msg: String) -> ! {
    eprintln!("{msg}");
    std::process::exit(1)
}

fn tenants_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("TENANTS_DIR").filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
}

fn require_tenants_dir() -> std::path::PathBuf {
    tenants_dir().unwrap_or_else(|| die("Set TENANTS_DIR to the fleet root for tenant commands".into()))
}

fn abs_path(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(path) }
    })
}

/// Commands that work on files/fleets without opening the single database.
/// Returns true when a command ran.
fn run_file_command(args: &[String]) -> bool {
    match args.get(1).map(String::as_str) {
        Some("help" | "--help" | "-h") => println!("{USAGE}"),
        Some("backup") => {
            let Some(dest) = args.get(2) else { die("Usage: roomlog backup <destination.db>".into()) };
            let source = std::env::var("DATABASE_PATH").unwrap_or_else(|_| default_db_path());
            match db::backup_database_file(std::path::Path::new(&source), std::path::Path::new(dest)) {
                Ok(()) => println!("Snapshot of {source} written to {dest}"),
                Err(e) => die(e),
            }
        }
        Some("check-database") => {
            let Some(path) = args.get(2) else { die("Usage: roomlog check-database <file.db>".into()) };
            match db::check_database_file(std::path::Path::new(path)) {
                Ok(v) => println!("{path} is a valid RoomLog database (schema version {v})"),
                Err(e) => die(e),
            }
        }
        Some("tenant-create") => {
            let Some(slug) = args.get(2) else { die("Usage: roomlog tenant-create <slug>".into()) };
            let password = std::env::var("ADMIN_PASSWORD")
                .unwrap_or_else(|_| die("Set ADMIN_PASSWORD for the tenant's first admin account".into()));
            match fleet::create_tenant(&require_tenants_dir(), slug, &password) {
                Ok(p) => println!("Tenant {slug} created at {}", p.display()),
                Err(e) => die(e),
            }
        }
        Some("tenant-list") => {
            for slug in fleet::list_tenants(&require_tenants_dir()) {
                println!("{slug}");
            }
        }
        Some("tenant-backup") => {
            let (Some(slug), Some(dest)) = (args.get(2), args.get(3)) else {
                die("Usage: roomlog tenant-backup <slug> <destination.db>".into())
            };
            let source = fleet::tenant_db_path(&require_tenants_dir(), slug);
            match db::backup_database_file(&source, std::path::Path::new(dest)) {
                Ok(()) => println!("Snapshot of tenant {slug} written to {dest}"),
                Err(e) => die(e),
            }
        }
        Some("tenant-import") => {
            let (Some(slug), Some(source)) = (args.get(2), args.get(3)) else {
                die("Usage: roomlog tenant-import <slug> <file.db>".into())
            };
            match fleet::import_tenant(&require_tenants_dir(), slug, std::path::Path::new(source)) {
                Ok(p) => println!("Tenant {slug} imported at {}", p.display()),
                Err(e) => die(e),
            }
        }
        Some("tenant-backup-all") => {
            let Some(dest) = args.get(2) else { die("Usage: roomlog tenant-backup-all <dest-dir>".into()) };
            match fleet::backup_all(&require_tenants_dir(), std::path::Path::new(dest)) {
                Ok(entries) => println!("Snapshotted {} tenants into {dest} (manifest.json written)", entries.len()),
                Err(e) => die(e),
            }
        }
        Some("verify-backups") => {
            let Some(dir) = args.get(2) else { die("Usage: roomlog verify-backups <backup-dir>".into()) };
            match fleet::verify_backups(std::path::Path::new(dir)) {
                Ok(entries) => println!("OK: {} backups in {dir} verify against the manifest", entries.len()),
                Err(e) => die(e),
            }
        }
        Some("restore-drill") => {
            let Some(target) = args.get(2) else { die("Usage: roomlog restore-drill <slug|file.db>".into()) };
            let report = match tenants_dir() {
                Some(dir) => fleet::restore_drill(&dir, target),
                None => fleet::restore_drill_file(std::path::Path::new(target)),
            };
            match report {
                Ok(r) => println!(
                    "Restore drill OK for {}: schema v{}, {} rooms, {} users, {} events",
                    r.slug, r.schema_version, r.rooms, r.users, r.events
                ),
                Err(e) => die(e),
            }
        }
        Some("tenant-versions") => {
            let dir = require_tenants_dir();
            for slug in fleet::list_tenants(&dir) {
                let version = db::schema_version_of(&fleet::tenant_db_path(&dir, &slug));
                let state = if fleet::is_suspended(&dir, &slug) { " suspended" } else { "" };
                println!("{slug} v{}{state}", version.map_or_else(|| "?".into(), |v| v.to_string()));
            }
        }
        Some("backup-config") => {
            let Some(replica) = args.get(2) else { die("Usage: roomlog backup-config <replica-url>".into()) };
            match tenants_dir() {
                Some(dir) => {
                    let dir = abs_path(&dir);
                    let meta = dir.parent().map(|p| p.join("litestream-meta")).unwrap_or_else(|| dir.join(".litestream-meta"));
                    print!("{}", fleet::litestream_config_fleet(&dir, &meta, replica));
                }
                None => {
                    let source = std::env::var("DATABASE_PATH").unwrap_or_else(|_| default_db_path());
                    print!("{}", fleet::litestream_config_single(&abs_path(std::path::Path::new(&source)), replica));
                }
            }
        }
        _ => return false,
    }
    true
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();

    let args: Vec<String> = std::env::args().collect();
    if run_file_command(&args) {
        return;
    }

    let cookie_secure = matches!(std::env::var("COOKIE_SECURE").as_deref(), Ok("1" | "true" | "yes"));
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());

    if args.get(1).is_none()
        && let Some(dir) = tenants_dir() {
            if std::env::var_os("DATABASE_PATH").is_some_and(|v| !v.is_empty()) {
                die("Set only one of TENANTS_DIR (fleet) and DATABASE_PATH (single church)".into());
            }
            std::fs::create_dir_all(&dir).unwrap_or_else(|e| die(format!("Cannot create {}: {e}", dir.display())));
            let base_domain = std::env::var("BASE_DOMAIN").ok().map(|d| d.trim().trim_start_matches('.').to_ascii_lowercase()).filter(|d| !d.is_empty());
            let max_open = std::env::var("MAX_OPEN_TENANTS").ok().and_then(|v| v.parse().ok()).unwrap_or(128);
            let listener = tokio::net::TcpListener::bind(&bind).await.unwrap_or_else(|e| die(format!("Failed to bind {bind}: {e}")));
            tracing::info!("RoomLog fleet listening on http://{bind} (tenants: {})", dir.display());
            axum::serve(listener, fleet::fleet_app(fleet::FleetState::new(dir, base_domain, cookie_secure, max_open)))
                .await
                .expect("server error");
            return;
        }

    let db_path = std::env::var("DATABASE_PATH").unwrap_or_else(|_| default_db_path());
    let conn = db::open(&db_path).unwrap_or_else(|e| {
        eprintln!("Failed to open database {db_path}: {e}");
        std::process::exit(1);
    });
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

    if args.get(1).map(String::as_str) == Some("reset-password") {
        let Some(username) = args.get(2) else {
            eprintln!("Usage: roomlog reset-password <username>   (new password is read from stdin)");
            std::process::exit(2);
        };
        eprint!("New password for {username}: ");
        let mut password = String::new();
        if std::io::stdin().read_line(&mut password).is_err() {
            std::process::exit(1);
        }
        match auth::reset_password(&conn, username, password.trim_end_matches(['\r', '\n'])) {
            Ok(true) => println!("Password reset for {username}; the account is enabled."),
            Ok(false) => {
                eprintln!("No account called {username}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if let Some(other) = args.get(1) {
        eprintln!("Unknown command {other:?}\n{USAGE}");
        std::process::exit(2);
    }

    bootstrap_admin(&conn);

    let state = api::AppState::new(conn, cookie_secure);
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap_or_else(|e| {
        eprintln!("Failed to bind {bind}: {e}");
        std::process::exit(1);
    });
    tracing::info!("RoomLog listening on http://{bind} (database: {db_path})");
    axum::serve(listener, app(state)).await.expect("server error");
}
