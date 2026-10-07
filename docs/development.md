# Development Guide

## Build & Test Commands

```bash
# Debug build
cargo build

# Release build (optimized, stripped)
cargo build --release

# Run all tests
cargo test

# Run specific test
cargo test test_full_migration

# Run with output
cargo test -- --nocapture

# Check formatting
cargo fmt --check

# Lint
cargo clippy -- -D warnings

# Generate docs
cargo doc --open
```

There is no local hook — run these before committing:

```bash
cargo fmt --check --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --locked
```

## Per-File Map

| File | Responsibility | Key Types/Functions |
|------|----------------|---------------------|
| `src/main.rs` | CLI entry, argument parsing, logging setup | `Args`, `main()` (prints `Display` error, exits 1), `run()`, `init_logging()`, `resolve_profile()`, `list_profiles_cmd()` |
| `src/lib.rs` | Public API re-exports | `pub use` of the items with consumers: `migrate`, `migrate_with_spec`, `MigrationContext`, `MigrationSpec`, `MigrationStats`, `discover_profiles`, `Browser`, `Profile` |
| `src/error.rs` | Error enum, `Result` alias | `Error`, `Result` |
| `src/models.rs` | Data structures, serialization, UTF-8 handling | `Origin`, `Place`, `Visit`, `VisitType`, `MigrationStats`, `Microseconds`, `get_text_lossy()`, `get_text_lossy_required()` |
| `src/profile.rs` | Profile discovery & validation | `Browser`, `Profile`, `discover_profiles()`, `get_default_profile()`, `find_profile()` |
| `src/db.rs` | SQLite connections, schema, transactions | `DbContext` (seam: `open_source_db()`, `open_dest_db()` (fail-fast: rejects a silently read-only open with `Error::ReadOnlyDestination`), `ensure_schema()`, `with_txn()`, `with_dry_run_txn()`, `as_conn()`, `table_counts()`, `restore_pragmas()`), `AutoRollback`, plus free helpers used by the seam and tests: `open_source_db()`, `open_dest_db()`, `ensure_schema()`, `restore_safe_pragmas()`, `get_table_counts()`, `CREATE_TABLES_SQL`, `SRC_OPEN_FLAGS`, `DST_OPEN_FLAGS` |
| `src/dedup.rs` | Merge/deduplication algorithms | `DedupContext` (seam: `load_from()`, `upsert_origin()`, `upsert_place()`, `upsert_visit()`), `OriginMap`, `PlaceMap`, `VisitDedupSet`, `recalc_frecency()`, `update_meta()` |
| `src/migrate.rs` | Migration orchestration | `MigrationSpec`, `MigrationContext`, `migrate_with_spec()`, `migrate()`, `migrate_with_context()` (private core), `migrate_origins_phase()`, `migrate_places_phase()`, `migrate_visits_phase()` (private), `validate_migration()` |
| `tests/integration_test.rs` | End-to-end tests | `test_full_migration()`, `test_merge_deduplication()`, `test_discover_profiles()`, `test_migration_stats_display()` |

## Timing Constants

| Constant | Location | Value | Purpose |
|----------|----------|-------|---------|
| Progress bar | `migrate.rs` | `src_origins + src_places + src_visits` (counted before the transaction) | Updates per row |

> Note: The `BATCH_SIZE` and `MAX_TX_ROWS` constants were removed during cleanup. The current implementation streams row-by-row within a single transaction. For >1M rows, consider re-adding batched commits.

## Testing Conventions

- **Unit tests**: In `#[cfg(test)]` modules alongside code (`db.rs` DbContext tests, `dedup.rs`, `migrate.rs`)
- **Integration tests**: `tests/integration_test.rs` — uses `tempdir`, creates real SQLite files, exercises full pipeline
- **Test isolation**: Each test creates fresh temp directories; no shared state
- **Dry-run tests**: Verify read counts without writes
- **Merge tests**: Pre-populate destination, verify `visit_count` summation and `last_visit_date` max logic
- **Empty DB migration test**: Verify migration works when destination database is empty (tests `test_full_migration`)

Run integration tests with:
```bash
cargo test --test integration_test -- --nocapture
```

## Extension Recipes

### Add bookmark migration

1. Add `moz_bookmarks` and `moz_bookmarks_deleted` to `CREATE_TABLES_SQL` in `db.rs`
2. Add `Bookmark` struct to `models.rs` with `from_row`/`to_insert_params`
3. Add `BookmarkMap` type and an `upsert_bookmark` method on `DedupContext` in `dedup.rs`
   - Dedup key: `(fk, type)` for bookmarks, `guid` for folders
   - Parent folder ID remapping via folder map
4. Add a `migrate_bookmarks_phase` fn in `migrate.rs` after Places, before Visits
   - Requires folder tree walk to resolve parent IDs
5. Add `--include-bookmarks` CLI flag in `main.rs`

### Add keyword/search engine migration

Similar pattern: `moz_keywords` table, `Keyword` model, dedup on `(keyword, place_id)`.

### Support Thunderbird / other Gecko apps

- Extend `Browser` enum in `profile.rs`
- Add qualifier in `project_dirs_qualifier()`
- Verify `places.sqlite` schema compatibility (usually identical)

### Async migration (for GUI progress)

Current sync model blocks thread. For async:
1. Replace `rusqlite` with `sqlx` + `sqlite` (requires async runtime)
2. Or keep `rusqlite` in blocking thread pool (`tokio::task::spawn_blocking`)
3. Stream progress via `mpsc` channel to UI

### Custom frecency algorithm

Firefox's actual frecency is more complex (decay curves, typed boost). Current simplified version in `recalc_frecency`:

```sql
frecency = visit_count * 1000 / (days_since_last_visit + 1)
```

To match Firefox exactly, port the C++ algorithm from `mozilla-central` or call into `libplaces` via FFI (complex).

### UTF-8 handling in Firefox data

Firefox's `places.sqlite` may contain invalid UTF-8 in TEXT columns. The tool uses `row.get_ref()` with `ValueRef::Text`/`ValueRef::Blob` to access raw bytes, then applies `String::from_utf8_lossy()` for lossy conversion. See `models.rs`: `get_text_lossy()` and `get_text_lossy_required()`.

### Empty destination database migration

The tool now handles migration to an empty LibreWolf profile correctly. The fix:

1. `DedupContext::upsert_place` records a source-place-id → destination-place-id mapping internally (`place_id_map`)
2. `DedupContext::upsert_visit` rewrites `place_id` through that map before computing the dedup key and inserting
3. This ensures visits can reference newly created places even when the destination database starts empty

See `dedup.rs`: `DedupContext::place_id_map` (populated by `upsert_place`, consumed by `upsert_visit`).

## Cross-Platform Notes

### Windows
- Default profiles: `%APPDATA%\Mozilla\Firefox\Profiles\`, `%APPDATA%\librewolf\Profiles\`
- Lock files: `parent.lock` (Firefox), `.parentlock` (LibreWolf)
- Tested on Windows 10/11

### Linux
- Default profiles: `~/.mozilla/firefox/`, `~/.librewolf/`
- Lock files: `.parentlock` (both)
- `$XDG_CONFIG_HOME` respected via `directories-next`

### macOS
- Default profiles: `~/Library/Application Support/Firefox/Profiles/`, `~/Library/Application Support/librewolf/Profiles/`
- Lock files: `.parentlock`
- App sandbox may require Full Disk Access for `~/Library`

### Path handling
All paths use `std::path::PathBuf` — no platform-specific string manipulation. `directories-next` abstracts config/data dirs.

## Debugging Tips

### Inspect database during migration
```bash
# Copy DB mid-migration (if not in transaction)
sqlite3 places.sqlite ".schema"
sqlite3 places.sqlite "SELECT * FROM moz_places LIMIT 5;"
```

### Enable trace logging
```bash
RUST_LOG=trace fox2wolf --dry-run
```

### Profile lock detection
```bash
# Check lock files
ls -la ~/.mozilla/firefox/*.default*/parent.lock
ls -la ~/.librewolf/*.default*/.parentlock
```

### "Destination database is read-only" error

SQLite silently downgrades a read-write open to read-only when `CreateFile`/`open` for writing fails (file locked by a running LibreWolf, read-only attribute, security software, or process-level write restrictions), and only reports the failure at the first write. `DbContext::open_dest_db` guards against this by checking `Connection::is_readonly` immediately after opening, so the run stops with the path and likely causes before any PRAGMA runs. Close LibreWolf, verify the file attribute (`attrib +R`), and retry.

### Verify schema compatibility
```bash
sqlite3 ~/.mozilla/firefox/*.default*/places.sqlite ".schema" > ff_schema.sql
sqlite3 ~/.librewolf/*.default*/places.sqlite ".schema" > lw_schema.sql
diff ff_schema.sql lw_schema.sql
```

## Release Checklist

- [ ] Update version in `Cargo.toml`
- [ ] `cargo test --all-targets`
- [ ] `cargo clippy -- -D warnings`
- [ ] `cargo fmt --check`
- [ ] `cargo build --release`
- [ ] Run `fox2wolf --dry-run` against a real Firefox/LibreWolf profile pair; confirm the reported counts look right
- [ ] Test binary on target platforms (Windows, Linux, macOS)
- [ ] Publish release manually: `gh release create vX.Y.Z <binaries>` (or via web UI); no release workflow is committed in this repo