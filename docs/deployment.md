# Deployment: self-hosted and hosted from one install

Status: fleet mode, the hosted control-plane API, backup sets with restore
drills, verified Litestream replication, and cells (directory, assignment,
moves, edge routes) are built. A 100k-tenant load run is recorded under
"Measured capacity" below. Public signup/billing front-ends remain follow-on
work on top of the control-plane API (see "Roadmap").

## The contract

1. **One church = one SQLite file.** Everything a church owns — rooms, groups,
   events, bookings, loans, staff accounts, settings — lives in that file and
   nowhere else. Hosted-only state (billing, subdomain routing) is never church
   data and never lives in the file.
2. **Same binary everywhere.** Self-hosted and hosted run the identical
   `roomlog` binary and file format. Hosted is many isolated copies of the app,
   pooled by the fleet router — not a shared multi-tenant database.
3. **Moving is file moving.** A church leaves or joins hosted by exporting a
   consistent snapshot (`roomlog backup`, which uses `VACUUM INTO` so the copy
   is safe while the server runs) and importing that file on the other side.
   `roomlog check-database <file>` validates a file before use: integrity,
   schema version within the supported range, expected tables present.
4. **Versions.** Schema migrations stay append-only (`PRAGMA user_version`)
   with a backup taken before upgrade. The current release opens files from the
   previous release; `check-database` rejects files newer than the binary.
   Sessions do not survive an import — staff log in again; accounts do survive.

## Run modes

| Mode | Set | Serves |
|---|---|---|
| Single (default) | `DATABASE_PATH` (or default `roomlog.db`) | One church, exactly as before fleet mode existed |
| Fleet (hosted) | `TENANTS_DIR` | Many churches: `<TENANTS_DIR>/<slug>/roomlog.db`, chosen by subdomain |

Setting both `TENANTS_DIR` and `DATABASE_PATH` is refused at startup.

Fleet details:

* **Slug** = first DNS label (`st-marys.example.com` → `st-marys`). Slugs are
  1–63 chars of `a-z 0-9 -`, no leading/trailing hyphen; `www` is reserved.
  Optionally pin `BASE_DOMAIN=example.com` so only `*.<BASE_DOMAIN>` hosts
  resolve tenants; without it any `<slug>.<anything>` host resolves (handy for
  `st-marys.localhost` in development).
* **Unknown tenant → 404**, and nothing is created on disk. Tenants exist only
  when provisioned deliberately (`roomlog tenant-create <slug>`).
* **Lazy pooling.** A tenant database opens on its first request and is cached;
  the least-recently-used tenant closes when `MAX_OPEN_TENANTS` (default 128)
  is exceeded. Idle churches cost disk, not processes. The per-tenant write
  lock is unchanged, so approval clash-checks stay atomic per church while
  different churches never block each other.
* **One host owns a file.** WAL mode requires all processes for a database on
  one host and does not work over a network filesystem, so tenant files live on
  local disk of the fleet host currently serving them. Scale out by adding
  hosts/cells and moving tenant folders, never by sharing files.
* `GET /healthz` answers fleet liveness without a tenant; the same path inside
  a tenant (or single mode) answers app liveness.

## Deployment tiers

| Tier | Who | Shape |
|---|---|---|
| 0 — Bare self-host | Technical volunteer | Released binary + HTTPS reverse proxy (see README) |
| 1 — Packaged self-host (recommended) | Most self-hosters | `docker-compose.yml` in this repo: RoomLog + Caddy (automatic HTTPS), data in a named volume |
| 2 — Hosted pooled | First hosted region | Fleet mode on a small set of hosts behind an edge proxy; provision with the tenant CLI |
| 3 — Hosted cells | Path to 100k churches | Tier 2 copied per cell; a global slug→cell directory at the edge. Capacity = more cells, never a bigger shared database |

Per-church data is tiny (live/demo databases are ~150 KB) and mostly idle, so
100k churches is a fleet problem (provisioning, routing, backup, upgrades), not
a per-church throughput problem. Cells keep deploys and failures cell-local.

## Hosted control plane

Setting `FLEET_ADMIN_TOKEN` enables the operator API on a fleet process
(`CELL_ID` names the cell it serves, default `cell-1`). Every call needs
`Authorization: Bearer <token>`; without the variable the API does not exist
(404). Signup metadata (`church_name`, `contact_email`, provisioning time)
lives in `tenant.json` beside the database — hosted state never enters the
church's file, and the display name is also written to Settings inside it.

| Call | Effect |
|---|---|
| `POST /api/control/tenants` `{slug, church_name, contact_email, admin_password?}` | Provision a church (201). Omit the password to get a generated initial `admin` password returned once |
| `GET /api/control/tenants` | Tenants with size, schema version, suspension state |
| `GET /api/control/health` | Cell, tenant/open counts, total DB bytes (cheap stats only; use `/healthz` for load-balancer probes) |
| `POST /api/control/tenants/<slug>/suspend` / `/resume` | Cut or restore service (503 while suspended); data untouched |
| `POST /api/control/tenants/<slug>/backup` | Snapshot into `<TENANTS_DIR>/_backups/` |
| `POST /api/control/tenants/<slug>/restore-drill` | Prove the tenant snapshot restores; returns record counts |
| `DELETE /api/control/tenants/<slug>?confirm=<slug>` | Delete the tenant folder (no undo beyond backups) |

A public signup page, payments, and email delivery are deliberately outside
the app binary: they call this API. The binary never requires it.

## Cells

`CELLS_DIR` holds `directory.json` (`{cells, tenants: slug → cell}`) and one
`<cell>/tenants/` fleet root per cell. Run one fleet process per cell
(`TENANTS_DIR=<cell>/tenants CELL_ID=<cell>`). New churches are assigned to
the least-filled cell (`cell-tenant-create`, or assign then call that cell's
control API). `cell-routes <cell=host:port>...` renders the directory as
per-church reverse-proxy blocks for the edge, so a subdomain always lands on
the host owning its (host-local WAL) file. `cell-tenant-move` snapshots,
imports, verifies, carries metadata/suspension across, and only then deletes
the source — drain the tenant first; it is an offline operation.

## Operator commands

```sh
# Single mode (DATABASE_PATH selects the file, as for the server)
roomlog backup /backups/roomlog-2026-11-19.db   # consistent snapshot; refuses to overwrite
roomlog check-database some-file.db             # validate before import/use

# Fleet mode (TENANTS_DIR selects the fleet root)
TENANTS_DIR=/srv/roomlog ADMIN_PASSWORD=... roomlog tenant-create st-marys
TENANTS_DIR=/srv/roomlog roomlog tenant-list
TENANTS_DIR=/srv/roomlog roomlog tenant-backup st-marys /backups/st-marys.db
TENANTS_DIR=/srv/roomlog roomlog tenant-import st-pauls /imports/st-pauls.db
TENANTS_DIR=/srv/roomlog roomlog tenant-backup-all /backups/2026-11-20
roomlog verify-backups /backups/2026-11-20
TENANTS_DIR=/srv/roomlog roomlog restore-drill st-marys
TENANTS_DIR=/srv/roomlog roomlog tenant-versions
TENANTS_DIR=/srv/roomlog roomlog backup-config s3://mybucket/roomlog > litestream.yml

# Cells (CELLS_DIR): create, provision into least-filled, route, move, edges
CELLS_DIR=/srv/cells roomlog cell-create north
CELLS_DIR=/srv/cells ADMIN_PASSWORD=... roomlog cell-tenant-create st-marys
CELLS_DIR=/srv/cells roomlog cell-route st-marys
CELLS_DIR=/srv/cells roomlog cell-tenant-move st-marys south
CELLS_DIR=/srv/cells BASE_DOMAIN=example.com roomlog cell-routes north=10.0.0.1:8080 south=10.0.0.2:8080
```

`tenant-create` makes the folder and database and creates the first `admin`
account from `ADMIN_PASSWORD` (same bootstrap rule as single mode).
`tenant-import` validates the source first, refuses to replace an existing
tenant, migrates if needed, and clears sessions. Moving a tenant between hosts
or cells is `tenant-backup` on one side and `tenant-import` on the other —
the same path a church uses to move to/from self-hosting.

## Backups and upgrades

* Self-host: schedule `roomlog backup`, keep the snapshots off the machine.
  The pre-upgrade copy (`<file>.v<N>-backup-<time>`) is automatic but local —
  it is not a backup strategy by itself. Prove a snapshot any time with
  `roomlog restore-drill <file.db>` (imports into scratch and compares
  record counts; the live database is never touched).
* Hosted snapshot sets: `roomlog tenant-backup-all <dir>` snapshots every
  tenant and writes `manifest.json` (schema version + size per file);
  `roomlog verify-backups <dir>` re-validates the set against it;
  `roomlog restore-drill <slug>` (or the control-plane endpoint) drills one
  tenant. Schedule drills — a backup that has never been restored is a guess.
* Hosted continuous replication: `roomlog backup-config <replica-url>`
  prints a Litestream config. In fleet mode it is one watched directory entry
  (`pattern: "roomlog.db"`, recursive, `watch: true`, metadata kept outside
  tenant folders via `meta-dir`), so churches provisioned later replicate
  without regenerating. This exact generated config was verified end-to-end
  with Litestream v0.5.14: a room created through the live app replicated to
  a file replica, the tenant database was deleted, and
  `litestream restore -o <tenant>/roomlog.db file://<replica>/<slug>/roomlog.db`
  recovered it (drill-verified afterwards). For object storage, use its
  `s3://…` replica URL form instead. Retention in Litestream coarsens
  restore points over time, so choose snapshot retention against a stated
  recovery-point promise rather than accepting defaults blindly.
* Upgrades: roll the new binary to one host/cell first. Tenant files migrate
  lazily on open, each after its pre-upgrade backup, so a bad release halts
  with most files untouched. Rollback is the previous binary plus, for any
  migrated tenant that must go back, its pre-upgrade backup. Never run two
  binary versions against the same tenant file at once.

## Measured capacity (100k mostly-idle-tenant run)

Method (this repo, `tools/fleet_load.py`): a seeded template tenant
(172,032 bytes) reflink-copied to **100,000 real tenant folders** on one
release-binary fleet process; 300 active tenants, 32 concurrent clients,
30 s of mixed authenticated/public reads after one login each; loopback on a
CPU-rich dev host (166 vCPU) — absolute rates flatter the hardware, the
operational findings transfer.

| Scenario | Result |
|---|---|
| Pool covers active set (`MAX_OPEN_TENANTS=512`) | 184,726 requests, **0 errors**, 6,156 req/s, p50 3.9 ms, p95 14.2 ms, max RSS ≈168 MiB, 300 open |
| Pool below active set (`MAX_OPEN_TENANTS=64`) | 939 req/s, p50 33.9 ms — reopen thrash dominates; **size the pool above the simultaneous working set** |
| Idle / cold paths at 100k files | Process idle RSS ≈8.3 MiB; cold first-open of an untouched tenant ≈2.4 ms; unknown tenant 404 ≈0.5 ms; tenant directory scan (`tenant-list`) ≈318 ms; control-health aggregate ≈573 ms (operator call, not a probe) |
| Disk | 100,000 × demo-sized churches ≈ **16.0 GiB** apparent (172 KB each). Budget linearly with real data: at 1 MB/church it is ≈95 GiB per 100k — one volume, or cells well before that |

Per-open-tenant RSS worked out to roughly 0.5 MiB on top of the idle process,
so pool size is cheap insurance. The Sunday-correlated active fraction across
time zones is still an assumption to measure in production; cells exist so a
hot region never has to share one process with the whole fleet.

## Roadmap (not built yet)

On top of what exists, in order: public signup/billing front-end calling the
control-plane API; per-tenant quotas and rate limits in fleet dispatch;
custom domains beyond subdomains; automated canary orchestration around
`tenant-versions` (rollout itself is per-cell today); control-plane tenant
listing pagination for very large cells (health aggregates are already cheap,
listing opens each file for its schema version). None of these change the
portability contract.
