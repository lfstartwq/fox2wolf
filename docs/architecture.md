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
│  │  - open_source_db (read-only)   │   │
│  │  - open_dest_db (RW, optimized) │   │
│  │  - AutoRollback transaction     │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Deduplication (dedup.rs)       │   │
│  │  - OriginMap: (host,prefix)→id  │   │
│  │  - PlaceMap: (url_hash,url)→id  │   │
│  │  - VisitDedupSet: key set       │   │
│  │  - upsert_origin/place/visit    │   │
│  └─────────────────────────────────┘   │
│  ┌─────────────────────────────────┐   │
│  │  Migration Orchestration        │   │
│  │  - migrate_origins()            │   │
│  │  - migrate_places()             │   │
│  │  - migrate_visits()             │   │
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
- Builds destination dedup map from current transaction state
- Streams source rows ordered by primary key
- For each row: upsert with merge logic, record ID mapping
- Progress bar updates per row

## Concurrency Model

**Single-threaded, sequential execution.** Rationale:

- SQLite writes serialize anyway (WAL mode allows concurrent readers, single writer)
- ID remapping requires deterministic ordering — parallel streams would race on map mutations
- Progress reporting is simpler with single stream
- For >500k rows, bottlenecks are disk I/O and SQLite lock contention, not CPU

The `AutoRollback` wrapper ensures transaction atomicity. All three phases run in one implicit transaction (committed after Visits). `recalc_frecency` and metadata updates run after commit on the same connection.

## Data Flow

```
Source DB (RO)          Destination DB (RW)
─────────────────       ──────────────────
SELECT * FROM           BEGIN IMMEDIATE
moz_origins             INSERT OR MERGE
    │                       │
    ▼                       ▼
build OriginMap ◄─── OriginMap (in-memory)
    │                       │
    ▼                       ▼
SELECT * FROM           INSERT OR MERGE
moz_places              (with new GUID,
    │                       url_hash)
    ▼                       ▼
build PlaceMap ◄─── PlaceMap (in-memory)
    │                       │
    ▼                       ▼
SELECT * FROM           INSERT (deduped)
moz_historyvisits       (place_id rewritten)
    │                       │
    ▼                       ▼
build VisitDedupSet
    │
    ▼
COMMIT
    │
    ▼
UPDATE moz_places SET frecency=...
UPDATE moz_meta
```

## ID Remapping Strategy

| Table | Source PK | Dest PK | Mapping |
|-------|-----------|---------|---------|
| `moz_origins` | `id` | `id` | `(host, prefix)` unique key → new auto-increment |
| `moz_places` | `id` | `id` | `(url_hash, url)` dedup key → new auto-increment + new UUID v4 GUID |
| `moz_historyvisits` | `id` | `id` | Auto-increment reassigned; `place_id` rewritten via map from `migrate_places`; `from_visit` re-chained by visit_date ordering |

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
| `db.rs` owns PRAGMAs | Connection config belongs with connection creation; avoids scattering optimization flags |
| `dedup.rs` stateless functions | Pure functions on maps/transactions; easy to unit test with mock transactions |
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
Previously, migrating to an empty LibreWolf profile would fail the visit migration because `migrate_visits` built its `place_id` map from the destination database (which was empty). Fixed by having `migrate_places` return a `HashMap<old_place_id, new_place_id>` that `migrate_visits` uses to rewrite `place_id` before insertion. This ensures visits can reference newly created places even when the destination database starts empty.

## Security Considerations

- Source DB opened `READ_ONLY | NO_MUTEX` — no modification possible
- Destination DB uses `IMMEDIATE` transaction — prevents lock upgrade races
- No SQL interpolation — all params via `rusqlite::params![]`
- Path traversal: Profile paths resolved via `directories-next`, validated against `places.sqlite` existence