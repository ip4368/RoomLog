//! Cells: independent fleet stacks plus a slug → cell directory.
//!
//! Layout under `CELLS_DIR`:
//!
//! ```text
//! directory.json            {"cells": [...], "tenants": {"<slug>": "<cell>"}}
//! <cell>/tenants/<slug>/roomlog.db
//! ```
//!
//! A fleet process serves exactly one cell (`TENANTS_DIR=<cell>/tenants`,
//! `CELL_ID=<cell>`). The directory is the edge router's source of truth:
//! [`edge_routes`] renders it as reverse-proxy blocks so a church subdomain
//! always lands on the host that owns its file (WAL files are host-local).
//! Growth means adding cells, never enlarging a shared database.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db;
use crate::fleet;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Directory {
    pub cells: Vec<String>,
    #[serde(default)]
    pub tenants: BTreeMap<String, String>,
}

fn directory_path(cells_dir: &Path) -> PathBuf {
    cells_dir.join("directory.json")
}

pub fn cell_tenants_dir(cells_dir: &Path, cell: &str) -> PathBuf {
    cells_dir.join(cell).join("tenants")
}

pub fn valid_cell(cell: &str) -> bool {
    fleet::valid_slug(cell)
}

pub fn load_directory(cells_dir: &Path) -> Directory {
    std::fs::read_to_string(directory_path(cells_dir)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save_directory(cells_dir: &Path, directory: &Directory) -> Result<(), String> {
    std::fs::create_dir_all(cells_dir).map_err(|e| e.to_string())?;
    let tmp = cells_dir.join("directory.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(directory).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, directory_path(cells_dir)).map_err(|e| e.to_string())
}

pub fn create_cell(cells_dir: &Path, cell: &str) -> Result<PathBuf, String> {
    if !valid_cell(cell) {
        return Err(format!("Invalid cell id {cell:?} (use 1-63 chars of a-z, 0-9 and '-')"));
    }
    let mut directory = load_directory(cells_dir);
    if directory.cells.contains(&cell.to_string()) {
        return Err(format!("Cell {cell} already exists"));
    }
    let tenants = cell_tenants_dir(cells_dir, cell);
    std::fs::create_dir_all(&tenants).map_err(|e| e.to_string())?;
    directory.cells.push(cell.to_string());
    directory.cells.sort();
    save_directory(cells_dir, &directory)?;
    Ok(tenants)
}

/// `(cell, tenant_count)` per known cell, sorted by cell id.
pub fn list_cells(cells_dir: &Path) -> Vec<(String, usize)> {
    let directory = load_directory(cells_dir);
    directory.cells.iter().map(|c| (c.clone(), directory.tenants.values().filter(|v| *v == c).count())).collect()
}

pub fn route(cells_dir: &Path, slug: &str) -> Option<String> {
    load_directory(cells_dir).tenants.get(slug).cloned()
}

/// The least-filled cell (directory counts; ties go to the first cell id).
pub fn assign_cell(cells_dir: &Path) -> Result<String, String> {
    list_cells(cells_dir).into_iter().min_by_key(|(_, n)| *n).map(|(c, _)| c).ok_or_else(|| "No cells yet; create one first".into())
}

/// Provisions a tenant in the least-filled cell and records the route.
pub fn create_tenant_in_cells(
    cells_dir: &Path,
    slug: &str,
    admin_password: &str,
    church_name: &str,
    contact_email: &str,
) -> Result<String, String> {
    let mut directory = load_directory(cells_dir);
    if directory.tenants.contains_key(slug) {
        return Err(format!("Tenant {slug} already exists"));
    }
    let cell = assign_cell(cells_dir)?;
    fleet::create_tenant_with_meta(&cell_tenants_dir(cells_dir, &cell), slug, admin_password, church_name, contact_email)?;
    directory.tenants.insert(slug.to_string(), cell.clone());
    save_directory(cells_dir, &directory)?;
    Ok(cell)
}

/// Moves a tenant to another cell via snapshot + import (the same path a
/// church uses to move between hosted and self-hosted). Run it while the
/// source fleet is drained/stopped for that tenant: the source folder is
/// deleted only after the destination import verifies.
pub fn move_tenant(cells_dir: &Path, slug: &str, dest_cell: &str) -> Result<(String, String), String> {
    let mut directory = load_directory(cells_dir);
    let source_cell = directory.tenants.get(slug).cloned().ok_or_else(|| format!("Unknown tenant {slug}"))?;
    if !directory.cells.contains(&dest_cell.to_string()) {
        return Err(format!("Unknown cell {dest_cell}"));
    }
    if source_cell == dest_cell {
        return Err(format!("Tenant {slug} is already in {dest_cell}"));
    }
    let source_dir = cell_tenants_dir(cells_dir, &source_cell);
    let dest_dir = cell_tenants_dir(cells_dir, dest_cell);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let snapshot = cells_dir.join(format!(".move-{slug}-{}-{nanos}.db", std::process::id()));
    let moved = db::backup_database_file(&fleet::tenant_db_path(&source_dir, slug), &snapshot)
        .and_then(|()| fleet::import_tenant(&dest_dir, slug, &snapshot))
        .and_then(|_| {
            // Hosted metadata and suspension state travel with the church.
            let meta = fleet::read_meta(&source_dir, slug);
            fleet::write_meta(&dest_dir, &meta)?;
            fleet::set_suspended(&dest_dir, slug, fleet::is_suspended(&source_dir, slug))?;
            db::check_database_file(&fleet::tenant_db_path(&dest_dir, slug)).map(|_| ())
        });
    let _ = std::fs::remove_file(&snapshot);
    moved?;
    fleet::delete_tenant(&source_dir, slug)?;
    directory.tenants.insert(slug.to_string(), dest_cell.to_string());
    save_directory(cells_dir, &directory)?;
    Ok((source_cell, dest_cell.to_string()))
}

/// Renders the directory as Caddy reverse-proxy blocks, one per tenant.
/// `upstreams` maps cell id → fleet `host:port` for that cell.
pub fn edge_routes(cells_dir: &Path, base_domain: &str, upstreams: &BTreeMap<String, String>) -> Result<String, String> {
    let directory = load_directory(cells_dir);
    let mut out = String::from("# Generated by `roomlog cell-routes`. One block per church.\n");
    for (slug, cell) in &directory.tenants {
        let upstream = upstreams.get(cell).ok_or_else(|| format!("No upstream given for cell {cell}"))?;
        out.push_str(&format!("{slug}.{base_domain} {{\n    reverse_proxy {upstream}\n}}\n"));
    }
    Ok(out)
}
