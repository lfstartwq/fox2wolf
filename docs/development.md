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

A pre-commit hook runs formatting, linting, and tests. Enable it once per clone:

```bash
git config core.hooksPath .githooks
```

Skip it for a single commit with `git commit --no-verify`.

## Per-File Map

| File | Responsibility | Key Types/Functions |
|------|----------------|---------------------|
| `src/main.rs` | CLI entry, argument parsing, logging setup | `Args`, `main()`, `init_logging()`, `resolve_profile()`, `list_profiles_cmd()` |
| `src/lib.rs` | Public API re-exports | `pub use` of the six items with consumers: `migrate`, `MigrationContext`, `MigrationStats`, `discover_profiles`, `Browser`, `Profile` |
| `src/error.rs` | Error enum, `Result` alias | `Error`, `Result` |
| `src/models.rs` | Data structures, serialization, UTF-8 handling | `Origin`, `Place`, `Visit`, `VisitType`, `MigrationStats`, `Microseconds`, `get_text_lossy()`, `get_text_lossy_required()` |
| `src/profile.rs` | Profile discovery & validation | `Browser`, `Profile`, `discover_profiles()`, `get_default_profile()`, `find_profile()` |
| `src/db.rs` | SQLite connections, schema, transactions | `open_source_db()`, `open_dest_db()`, `ensure_schema()`, `AutoRollback`, `get_table_counts()` |
| `src/dedup.rs` | Merge/deduplication algorithms | `OriginMap`, `PlaceMap`, `VisitDedupSet`, `upsert_origin()`, `upsert_place()`, `upsert_visit()`, `recalc_frecency()`, `update_meta()` |
| `src/migrate.rs` | Migration orchestration | `MigrationContext`, `migrate()`, `migrate_origins()`, `migrate_places()` (returns `HashMap<old_place_id, new_place_id>`), `migrate_visits()` (accepts `place_id_map`), `validate_migration()` |
| `tests/integration_test.rs` | End-to-end tests | `test_full_migration()`, `test_merge_deduplication()`, `test_discover_profiles()`, `test_migration_stats_display()` |

## Timing Constants

| Constant | Location | Value | Purpose |
|----------|----------|-------|---------|
| Progress bar | `migrate.rs` | `total_items = origins + places + visits` | Updates per row |

> Note: The `BATCH_SIZE` and `MAX_TX_ROWS` constants were removed during cleanup. The current implementation streams row-by-row within a single transaction. For >1M rows, consider re-adding batched commits.

## Testing Conventions

- **Unit tests**: In `#[cfg(test)]` modules alongside code (`dedup.rs`, `migrate.rs`)
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
3. Add `BookmarkMap` type and `build_bookmark_map`/`upsert_bookmark` to `dedup.rs`
   - Dedup key: `(fk, type)` for bookmarks, `guid` for folders
   - Parent folder ID remapping via folder map
4. Add `migrate_bookmarks` phase in `migrate.rs` after Places, before Visits
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

1. `migrate_places` returns a `HashMap<old_place_id, new_place_id>` mapping
2. `migrate_visits` receives this map and uses it to rewrite `place_id` before insertion
3. This ensures visits can reference newly created places even when the destination database starts empty

See `migrate.rs`: `migrate_places()` (returns `HashMap<i64, i64>`) and `migrate_visits()` (accepts `&HashMap<i64, i64>`).

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