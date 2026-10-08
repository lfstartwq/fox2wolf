# Architecture

## Overview

`fox2wolf` migrates Firefox `places.sqlite` history to LibreWolf. The core challenge is handling ID remapping and merge deduplication between two SQLite databases with identical schemas but different primary key assignments.

## Component Diagram

```
┌─────────────────┐     ┌─────────────────┐
│  Firefox        │     │  LibreWolf      │
│  Profile Dir    │     │  Profile Dir    │
│  places.sqlite  │     │  places.sqlite  │
└────────┬────────┘     └────────┬────────┘
         │                       │
         ▼                       ▼
┌─────────────────────────────────────────┐
│            Migration Engine             │
│  ┌─────────────────────────────────┐   │
│  │  Profile Discovery (profile.rs) │   │
│  │  - reads profiles.ini           │   │
│  │  - resolves default/name/path   │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Database Layer (db.rs)         │   │
│  │  - DbContext seam:              │   │
│  │    open_source_db (RO)          │   │
│  │    open_dest_db / with_txn      │   │
│  │    ensure_schema / as_conn      │   │
│  │    with_dry_run_txn             │   │
│  │  - AutoRollback (internal)      │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Deduplication (dedup.rs)       │   │
│  │  - DedupContext seam:           │   │
│  │    load_from / upsert_origin    │   │
│  │    upsert_place / upsert_visit  │   │
│  │  - hides OriginMap/PlaceMap/    │   │
│  │    VisitDedupSet/place_id_map/  │   │
│  │    origin_id_map                │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Migration Orchestration        │   │
│  │  - MigrationContext             │   │
│  │  - migrate()                    │   │
│  │    (origins → places → visits)  │   │
│  │  - validate_migration()         │   │
│  └─────────────────────────────────┘   │
└─────────────────────────────────────────┘
```

## Key Routing Order

The migration executes in strict dependency order:

1. **Origins first** — `moz_places.origin_id` FK references `moz_origins.id`
2. **Places second** — `moz_historyvisits.place_id` FK references `moz_places.id`
3. **Visits last** — no outgoing FKs, depends on Places map

Each phase:
- Streams source rows ordered by primary key
- For each row: upsert through `DedupContext` with merge logic, record ID mapping
- Progress bar updates per row

Dedup state itself is seeded **once** before the phases, from the destination DB inside the transaction (`DedupContext::load_from`), then kept up to date by the upserts — there is no per-phase map rebuild.

## Concurrency Model

**Single-threaded, sequential execution.** Rationale:

- SQLite writes serialize anyway (WAL mode allows concurrent readers, single writer)
- ID remapping requires deterministic ordering — parallel streams would race on map mutations
- Progress reporting is simpler with single stream
- For >500k rows, bottlenecks are disk I/O and SQLite lock contention, not CPU

The `DbContext::with_txn` helper (built on `AutoRollback`) ensures transaction atomicity: it commits on success and rolls back on error. Dry-run goes through `DbContext::with_dry_run_txn`, which runs the same closure but always rolls back. All three phases plus `recalc_frecency` and metadata updates run inside that single transaction.

## Data Flow

```
Source DB (RO)          Destination DB (RW)
─────────────────       ──────────────────
BEGIN IMMEDIATE
load_from(): seed dedup maps from
destination rows inside the txn
(OriginMap, PlaceMap,     ◄─── OriginMap /
VisitDedupSet)                 PlaceMap /
    │                           VisitDedupSet
    ▼
explicit-column SELECT      upsert_origin
FROM moz_origins             INSERT or reuse,
(ORDER BY id) ────────────► record origin_id_map
    │
    ▼
explicit-column SELECT      upsert_place
FROM moz_places              INSERT or MERGE
(ORDER BY id) ────────────► (new GUID, url_hash,
    │                        source→dest origin_id)
    ▼
explicit-column SELECT      upsert_visit
FROM moz_historyvisits       place_id rewritten via
(ORDER BY id) ────────────► place_id_map, dedup key
    │                        via Visit::key_of
    ▼
recalc_frecency(dst_tx)     UPDATE moz_places SET frecency=...
update_meta(dst_tx, &stats) UPDATE moz_meta
    │
    ▼
COMMIT  (rolled back instead on error or --dry-run)
```

## ID Remapping Strategy

| Table | Source PK | Dest PK | Mapping |
|-------|-----------|---------|---------|
| `moz_origins` | `id` | `id` | `(host, prefix)` unique key → new auto-increment |
| `moz_places` | `id` | `id` | `(url_hash, url)` dedup key → new auto-increment + new UUID v4 GUID |
| `moz_historyvisits` | `id` | `id` | Auto-increment reassigned; `place_id` rewritten via `DedupContext.place_id_map` (recorded by `upsert_place`); `from_visit` copied verbatim from the source row |

## Merge Deduplication Logic

**Origins**: Unique on `(host, prefix)`. If exists, keep existing (no data to merge).

**Places**: Dedup key = `url_hash:url`. On collision:
- `visit_count += incoming.visit_count`
- `hidden = hidden OR incoming.hidden`
- `typed = MAX(typed, incoming.typed)`
- `foreign_count += incoming.foreign_count`
- `last_visit_date = MAX(last_visit_date, incoming.last_visit_date)`
- `origin_id = COALESCE(new_origin_id, existing.origin_id)`
- `recalc_frecency = 1` (trigger recalc later)

**Visits**: Dedup key = `place_id:visit_date:visit_type`. Skip if exists.

## Why This Layout?

| Decision | Reason |
|----------|--------|
| Separate `profile.rs` | Profile discovery is browser-specific, reusable, testable in isolation |
| `db.rs` owns PRAGMAs | Connection config belongs with connection creation; both source and destination open through `DbContext`, so connection flags, PRAGMAs and transaction semantics never leak into migration code |
| `dedup.rs` behavior object | `DedupContext` encapsulates the dedup maps behind `upsert_*` methods; orchestration sees behavior, not map plumbing |
| `migrate.rs` orchestrates | Single entry point, clear phase boundaries, handles CLI concerns (dry-run, confirm, progress) |
| Models in `models.rs` | Shared DTOs; `VisitType` as newtype wrapper avoids enum cast issues |

## Known Limitations

### Frecency Calculation Deviation
The frecency recalculation in `dedup.rs` uses a simplified formula:
```sql
frecency = visit_count * 1000 / (days_since_last_visit + 1)
```

Firefox's actual algorithm (from `mozilla-central`) uses exponential decay curves with typed visit boosts and bucket-based aging. This simplified version produces reasonable ordering for most users but may not match Firefox's exact frecency values. For exact compatibility, porting the C++ algorithm or linking against `libplaces` would be required.

### UTF-8 Handling in Firefox Data
Firefox's `places.sqlite` may contain invalid UTF-8 sequences in TEXT columns (notably `description`, `title`, `site_name`). The tool handles this by using `row.get_ref()` to access raw `ValueRef::Text` and `ValueRef::Blob` bytes, then applying `String::from_utf8_lossy()` for lossy conversion. See `models.rs`: `get_text_lossy()` and `get_text_lossy_required()`.

### Empty LibreWolf Profile Migration
Previously, migrating to an empty LibreWolf profile would fail the visit migration because the `place_id` map was built from the destination database (which was empty). Fixed by having `DedupContext::upsert_place` record a source-id → destination-id mapping as places are migrated, and `DedupContext::upsert_visit` rewrite `place_id` through that map before insertion. This ensures visits can reference newly created places even when the destination database starts empty.

## Security Considerations

- Source DB opened `READ_ONLY | NO_MUTEX` — no modification possible
- Destination open is verified with `is_readonly` right after opening; SQLite's silent read-only downgrade (write `CreateFile` denied) fails fast with `Error::ReadOnlyDestination` instead of surfacing later at the first write statement
- Destination DB uses `IMMEDIATE` transaction — prevents lock upgrade races
- No SQL interpolation — all params via `rusqlite::params![]`
- Path traversal: Profile paths resolved via `directories-next`, validated against `places.sqlite` existence