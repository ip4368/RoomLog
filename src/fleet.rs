//! Fleet mode: many churches, one process, one database file per church.
//!
//! The single-church app ([`crate::app`]) is reused unchanged per tenant.
//! This module only resolves the tenant from the request host, opens tenant
//! databases lazily (bounded cache, least-recently-used eviction), and
//! provides the provisioning operations the operator CLI uses. Unknown
//! tenants are refused without creating anything on disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

use crate::api::AppState;
use crate::auth;
use crate::db;

pub const TENANT_DB: &str = "roomlog.db";
/// Hosted-only metadata beside (never inside) the church's database file.
pub const TENANT_META: &str = "tenant.json";
/// Marker file: presence suspends the tenant (dispatch returns 503).
pub const SUSPENDED_MARKER: &str = "SUSPENDED";
const RESERVED_SLUGS: &[&str] = &["www"];

/// A tenant slug is a single DNS label: it names both the subdomain and the
/// folder on disk, so anything that could escape either is refused.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 63
        && !RESERVED_SLUGS.contains(&slug)
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Resolves the tenant slug from a `Host` value (port allowed). With a base
/// domain, only `<slug>.<base>` resolves; without one, the first label of any
/// dotted host resolves (e.g. `st-marys.localhost` in development).
pub fn tenant_from_host(host: &str, base_domain: Option<&str>) -> Option<String> {
    let host = host.trim().to_ascii_lowercase();
    let host = host.rsplit_once(':').map_or(host.as_str(), |(h, port)| {
        if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) { h } else { host.as_str() }
    });
    // IP literals are never tenant hosts.
    if host.starts_with('[') || (host.contains('.') && host.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))) {
        return None;
    }
    let slug = match base_domain {
        Some(base) => host.strip_suffix(base)?.strip_suffix('.')?,
        None => host.split_once('.')?.0,
    };
    valid_slug(slug).then(|| slug.to_string())
}

pub fn tenant_db_path(tenants_dir: &Path, slug: &str) -> PathBuf {
    tenants_dir.join(slug).join(TENANT_DB)
}

pub fn tenant_dir(tenants_dir: &Path, slug: &str) -> PathBuf {
    tenants_dir.join(slug)
}

/// Hosted-only tenant metadata. Church data stays in `roomlog.db`; this file
/// holds what only the host knows (contact, provisioning time).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TenantMeta {
    pub slug: String,
    #[serde(default)]
    pub church_name: String,
    #[serde(default)]
    pub contact_email: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct TenantInfo {
    pub slug: String,
    pub church_name: String,
    pub contact_email: String,
    pub created_at: String,
    pub suspended: bool,
    pub db_bytes: u64,
    pub schema_version: Option<i64>,
}

fn meta_path(tenants_dir: &Path, slug: &str) -> PathBuf {
    tenant_dir(tenants_dir, slug).join(TENANT_META)
}

pub fn read_meta(tenants_dir: &Path, slug: &str) -> TenantMeta {
    std::fs::read_to_string(meta_path(tenants_dir, slug))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| TenantMeta { slug: slug.to_string(), ..Default::default() })
}

pub fn write_meta(tenants_dir: &Path, meta: &TenantMeta) -> Result<(), String> {
    let json = serde_json::to_string_pretty(meta).map_err(|e| e.to_string())?;
    std::fs::write(meta_path(tenants_dir, &meta.slug), json).map_err(|e| e.to_string())
}

pub fn is_suspended(tenants_dir: &Path, slug: &str) -> bool {
    tenant_dir(tenants_dir, slug).join(SUSPENDED_MARKER).exists()
}

pub fn set_suspended(tenants_dir: &Path, slug: &str, suspended: bool) -> Result<(), String> {
    if !tenant_db_path(tenants_dir, slug).is_file() {
        return Err(format!("Unknown tenant {slug}"));
    }
    let marker = tenant_dir(tenants_dir, slug).join(SUSPENDED_MARKER);
    if suspended {
        std::fs::write(&marker, b"suspended\n").map_err(|e| e.to_string())
    } else if marker.exists() {
        std::fs::remove_file(&marker).map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

pub fn tenant_info(tenants_dir: &Path, slug: &str) -> TenantInfo {
    let meta = read_meta(tenants_dir, slug);
    let db_path = tenant_db_path(tenants_dir, slug);
    TenantInfo {
        slug: slug.to_string(),
        church_name: meta.church_name,
        contact_email: meta.contact_email,
        created_at: meta.created_at,
        suspended: is_suspended(tenants_dir, slug),
        db_bytes: std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0),
        schema_version: db::schema_version_of(&db_path),
    }
}

/// Removes a tenant folder entirely. Callers must require explicit
/// confirmation; there is no undo beyond backups.
pub fn delete_tenant(tenants_dir: &Path, slug: &str) -> Result<(), String> {
    if !valid_slug(slug) || !tenant_db_path(tenants_dir, slug).is_file() {
        return Err(format!("Unknown tenant {slug}"));
    }
    std::fs::remove_dir_all(tenant_dir(tenants_dir, slug)).map_err(|e| e.to_string())
}

/// Creates a tenant folder and database with a first `admin` account.
pub fn create_tenant(tenants_dir: &Path, slug: &str, admin_password: &str) -> Result<PathBuf, String> {
    create_tenant_with_meta(tenants_dir, slug, admin_password, "", "")
}

/// Like [`create_tenant`], recording hosted signup metadata beside the
/// database and the display name inside it (Settings → church name).
pub fn create_tenant_with_meta(
    tenants_dir: &Path,
    slug: &str,
    admin_password: &str,
    church_name: &str,
    contact_email: &str,
) -> Result<PathBuf, String> {
    if !valid_slug(slug) {
        return Err(format!("Invalid tenant slug {slug:?} (use 1-63 chars of a-z, 0-9 and '-')"));
    }
    let contact_email = contact_email.trim();
    if !contact_email.is_empty() && !contact_email.contains('@') {
        return Err("Contact email must contain '@'".into());
    }
    let db_path = tenant_db_path(tenants_dir, slug);
    if db_path.exists() {
        return Err(format!("Tenant {slug} already exists"));
    }
    std::fs::create_dir_all(tenant_dir(tenants_dir, slug)).map_err(|e| e.to_string())?;
    let conn = db::open(db_path.to_string_lossy().as_ref()).map_err(|e| e.to_string())?;
    auth::create_user(&conn, "admin", "Administrator", admin_password, &[auth::Role::Admin]).map_err(|e| e.to_string())?;
    let church_name = church_name.trim();
    if !church_name.is_empty() {
        db::set_setting(&conn, "church_name", church_name).map_err(|e| e.to_string())?;
    }
    drop(conn);
    write_meta(
        tenants_dir,
        &TenantMeta {
            slug: slug.to_string(),
            church_name: church_name.to_string(),
            contact_email: contact_email.to_string(),
            created_at: db::fmt_dt(crate::api::now()),
        },
    )?;
    Ok(db_path)
}

/// Imports a validated database file as a new tenant. Sessions are cleared;
/// staff accounts and church data travel in the file.
pub fn import_tenant(tenants_dir: &Path, slug: &str, source: &Path) -> Result<PathBuf, String> {
    if !valid_slug(slug) {
        return Err(format!("Invalid tenant slug {slug:?} (use 1-63 chars of a-z, 0-9 and '-')"));
    }
    db::check_database_file(source)?;
    let db_path = tenant_db_path(tenants_dir, slug);
    if db_path.exists() {
        return Err(format!("Tenant {slug} already exists; refusing to replace it"));
    }
    std::fs::create_dir_all(db_path.parent().expect("tenant dir")).map_err(|e| e.to_string())?;
    db::backup_database_file(source, &db_path)?;
    let conn = db::open(db_path.to_string_lossy().as_ref()).map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM sessions", []).map_err(|e| e.to_string())?;
    let church_name = db::get_settings(&conn).map(|s| s.church_name).unwrap_or_default();
    drop(conn);
    write_meta(
        tenants_dir,
        &TenantMeta { slug: slug.to_string(), church_name, contact_email: String::new(), created_at: db::fmt_dt(crate::api::now()) },
    )?;
    Ok(db_path)
}

// ------------------------------------------------------------- backup sets

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupEntry {
    pub slug: String,
    pub file: String,
    pub schema_version: i64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupManifest {
    created_at: String,
    entries: Vec<BackupEntry>,
}

/// Snapshots every tenant into `dest_dir` (`<slug>.db` each) and writes
/// `manifest.json` recording version and size per file.
pub fn backup_all(tenants_dir: &Path, dest_dir: &Path) -> Result<Vec<BackupEntry>, String> {
    std::fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for slug in list_tenants(tenants_dir) {
        let file = format!("{slug}.db");
        let dest = dest_dir.join(&file);
        db::backup_database_file(&tenant_db_path(tenants_dir, &slug), &dest)?;
        entries.push(BackupEntry {
            slug,
            schema_version: db::check_database_file(&dest)?,
            bytes: std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
            file,
        });
    }
    let manifest = BackupManifest { created_at: db::fmt_dt(crate::api::now()), entries: entries.clone() };
    std::fs::write(dest_dir.join("manifest.json"), serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(entries)
}

/// Re-validates every file named by a backup set's manifest (integrity,
/// version, size). Fails naming the first bad tenant.
pub fn verify_backups(dest_dir: &Path) -> Result<Vec<BackupEntry>, String> {
    let raw = std::fs::read_to_string(dest_dir.join("manifest.json")).map_err(|e| format!("No readable manifest.json: {e}"))?;
    let manifest: BackupManifest = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    for entry in &manifest.entries {
        let path = dest_dir.join(&entry.file);
        let version = db::check_database_file(&path).map_err(|e| format!("{}: {e}", entry.slug))?;
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if version != entry.schema_version || bytes != entry.bytes {
            return Err(format!("{}: backup differs from manifest (version {version}, {bytes} bytes)", entry.slug));
        }
    }
    Ok(manifest.entries)
}

#[derive(Clone, Debug, Serialize)]
pub struct DrillReport {
    pub slug: String,
    pub schema_version: i64,
    pub rooms: i64,
    pub users: i64,
    pub events: i64,
}

fn db_counts(path: &Path) -> Result<(i64, i64, i64), String> {
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    let count = |table: &str| conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string());
    Ok((count("rooms")?, count("users")?, count("events")?))
}

fn drill_snapshot(snapshot: &Path, slug: &str) -> Result<DrillReport, String> {
    let expected = db_counts(snapshot)?;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let scratch = std::env::temp_dir().join(format!("roomlog-drill-{}-{nanos}", std::process::id()));
    let tenants = scratch.join("tenants");
    let result = import_tenant(&tenants, slug, snapshot).and_then(|restored| {
        let actual = db_counts(&restored)?;
        if actual != expected {
            return Err(format!("Restored counts {actual:?} differ from snapshot {expected:?}"));
        }
        Ok(DrillReport {
            slug: slug.to_string(),
            schema_version: db::check_database_file(&restored)?,
            rooms: actual.0,
            users: actual.1,
            events: actual.2,
        })
    });
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// Restore drill for one tenant: snapshot it, import the snapshot into a
/// scratch fleet, and confirm the restored data matches. The live tenant is
/// never touched and the scratch copy is deleted either way.
pub fn restore_drill(tenants_dir: &Path, slug: &str) -> Result<DrillReport, String> {
    if !valid_slug(slug) || !tenant_db_path(tenants_dir, slug).is_file() {
        return Err(format!("Unknown tenant {slug}"));
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let snapshot = std::env::temp_dir().join(format!("roomlog-drill-snap-{slug}-{}-{nanos}.db", std::process::id()));
    db::backup_database_file(&tenant_db_path(tenants_dir, slug), &snapshot)?;
    let report = drill_snapshot(&snapshot, slug);
    let _ = std::fs::remove_file(&snapshot);
    report
}

/// Restore drill for a standalone backup file (single-church snapshots).
pub fn restore_drill_file(backup: &Path) -> Result<DrillReport, String> {
    db::check_database_file(backup)?;
    drill_snapshot(backup, "drill")
}

fn yaml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// Litestream config replicating every tenant file under `tenants_dir`
/// (directory mode: new tenants are picked up by the watcher).
pub fn litestream_config_fleet(tenants_dir: &Path, meta_dir: &Path, replica_url: &str) -> String {
    format!(
        "# Generated by `roomlog backup-config`. Regenerate if paths change.\ndbs:\n  - dir: {}\n    pattern: \"roomlog.db\"\n    recursive: true\n    watch: true\n    meta-dir: {}\n    replica:\n      url: {}\n",
        yaml_str(&tenants_dir.to_string_lossy()),
        yaml_str(&meta_dir.to_string_lossy()),
        yaml_str(replica_url),
    )
}

/// Litestream config for one single-church database file.
pub fn litestream_config_single(db_path: &Path, replica_url: &str) -> String {
    format!(
        "# Generated by `roomlog backup-config`.\ndbs:\n  - path: {}\n    replicas:\n      - url: {}\n",
        yaml_str(&db_path.to_string_lossy()),
        yaml_str(replica_url),
    )
}

/// Tenant slugs that have a database on disk, sorted.
pub fn list_tenants(tenants_dir: &Path) -> Vec<String> {
    let mut slugs: Vec<String> = std::fs::read_dir(tenants_dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|slug| valid_slug(slug) && tenant_db_path(tenants_dir, slug).is_file())
                .collect()
        })
        .unwrap_or_default();
    slugs.sort();
    slugs
}

struct TenantEntry {
    router: Router,
    last_used: Instant,
}

#[derive(Clone)]
pub struct FleetState {
    tenants_dir: PathBuf,
    base_domain: Option<String>,
    cookie_secure: bool,
    max_open: usize,
    tenants: Arc<Mutex<HashMap<String, TenantEntry>>>,
}

impl FleetState {
    pub fn new(tenants_dir: PathBuf, base_domain: Option<String>, cookie_secure: bool, max_open: usize) -> Self {
        FleetState {
            tenants_dir,
            base_domain,
            cookie_secure,
            max_open: max_open.max(1),
            tenants: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The tenant's app, opened on first use. `None` when the tenant has no
    /// database on disk (nothing is created for unknown tenants).
    fn tenant_router(&self, slug: &str) -> Option<Router> {
        let mut tenants = self.tenants.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = tenants.get_mut(slug) {
            entry.last_used = Instant::now();
            return Some(entry.router.clone());
        }
        let db_path = tenant_db_path(&self.tenants_dir, slug);
        if !db_path.is_file() {
            return None;
        }
        let conn = db::open(db_path.to_string_lossy().as_ref()).ok()?;
        let router = crate::app(AppState::new(conn, self.cookie_secure));
        if tenants.len() >= self.max_open
            && let Some(oldest) = tenants.iter().min_by_key(|(_, e)| e.last_used).map(|(s, _)| s.clone()) {
                tenants.remove(&oldest);
            }
        tenants.insert(slug.to_string(), TenantEntry { router: router.clone(), last_used: Instant::now() });
        Some(router)
    }
}

async fn fleet_health() -> impl IntoResponse {
    axum::Json(serde_json::json!({ "ok": true, "mode": "fleet" }))
}

async fn dispatch(State(fleet): State<FleetState>, req: Request<Body>) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().host())
        .unwrap_or_default()
        .to_string();
    let Some(slug) = tenant_from_host(&host, fleet.base_domain.as_deref()) else {
        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "Unknown church" }))).into_response();
    };
    if tenant_db_path(&fleet.tenants_dir, &slug).is_file() && is_suspended(&fleet.tenants_dir, &slug) {
        return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({ "error": "This church's hosted service is suspended" })))
            .into_response();
    }
    match fleet.tenant_router(&slug) {
        Some(router) => router.oneshot(req).await.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        None => (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({ "error": "Unknown church" }))).into_response(),
    }
}

/// The fleet HTTP app: tenant dispatch for everything, plus a tenant-free
/// health check for load balancers.
pub fn fleet_app(state: FleetState) -> Router {
    Router::new().route("/healthz", get(fleet_health)).fallback(dispatch).with_state(state)
}
