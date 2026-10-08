// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! Database connection, schema, and transaction management.
//!
//! The intent-revealing seam for the migration is [`DbContext`]: it wraps a SQLite
//! connection and exposes only what migration needs — opening read-only / read-write
//! connections, idempotent schema creation, a transaction helper that commits or
//! rolls back as a unit, table-count queries, and safe-PRAGMA restoration. All
//! SQLite-specific details (PRAGMAs, WAL, mmap, connection flags) stay inside this
//! module, so the migration stays decoupled from SQLite plumbing.
//!
//! The `AutoRollback` type is the internal transaction mechanism used by
//! `DbContext::with_txn`; callers usually interact with the seam through `DbContext`
//! instead of constructing transactions by hand.

use crate::error::Result;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::path::Path;

/// Flags for opening the source database read-only.
const SRC_OPEN_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_ONLY
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_FULL_MUTEX);

/// Flags for opening the destination database read-write.
const DST_OPEN_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE
    .union(OpenFlags::SQLITE_OPEN_CREATE)
    .union(OpenFlags::SQLITE_OPEN_FULL_MUTEX);

/// SQL for creating all core history tables plus `moz_meta` (idempotent).
pub const CREATE_TABLES_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS moz_origins (
    id INTEGER PRIMARY KEY,
    prefix TEXT NOT NULL,
    host TEXT NOT NULL,
    frecency INTEGER NOT NULL,
    recalc_frecency INTEGER NOT NULL DEFAULT 0,
    alt_frecency INTEGER,
    recalc_alt_frecency INTEGER NOT NULL DEFAULT 0,
    block_until_ms INTEGER,
    block_pages_until_ms INTEGER,
    UNIQUE (host, prefix)
);

CREATE TABLE IF NOT EXISTS moz_places (
    id INTEGER PRIMARY KEY,
    url LONGVARCHAR,
    title LONGVARCHAR,
    rev_host LONGVARCHAR,
    visit_count INTEGER DEFAULT 0,
    hidden INTEGER DEFAULT 0 NOT NULL,
    typed INTEGER DEFAULT 0 NOT NULL,
    frecency INTEGER DEFAULT -1 NOT NULL,
    last_visit_date INTEGER,
    guid TEXT,
    foreign_count INTEGER DEFAULT 0 NOT NULL,
    url_hash INTEGER DEFAULT 0 NOT NULL,
    description TEXT,
    preview_image_url TEXT,
    site_name TEXT,
    origin_id INTEGER REFERENCES moz_origins(id),
    recalc_frecency INTEGER NOT NULL DEFAULT 0,
    alt_frecency INTEGER,
    recalc_alt_frecency INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS moz_historyvisits (
    id INTEGER PRIMARY KEY,
    from_visit INTEGER,
    place_id INTEGER,
    visit_date INTEGER,
    visit_type INTEGER,
    session INTEGER,
    source INTEGER DEFAULT 0 NOT NULL,
    triggeringPlaceId INTEGER
);

CREATE TABLE IF NOT EXISTS moz_meta (
    key TEXT PRIMARY KEY,
    value NOT NULL
) WITHOUT ROWID;

-- indexes
CREATE INDEX IF NOT EXISTS moz_places_url_hashindex ON moz_places (url_hash);
CREATE INDEX IF NOT EXISTS moz_places_hostindex ON moz_places (rev_host);
CREATE INDEX IF NOT EXISTS moz_places_frecencyindex ON moz_places (frecency);
CREATE INDEX IF NOT EXISTS moz_places_lastvisitdateindex ON moz_places (last_visit_date);
CREATE UNIQUE INDEX IF NOT EXISTS moz_places_guid_uniqueindex ON moz_places (guid);
CREATE INDEX IF NOT EXISTS moz_places_originidindex ON moz_places (origin_id);
CREATE INDEX IF NOT EXISTS moz_historyvisits_placedateindex ON moz_historyvisits (place_id, visit_date);
CREATE INDEX IF NOT EXISTS moz_historyvisits_fromindex ON moz_historyvisits (from_visit);
CREATE INDEX IF NOT EXISTS moz_historyvisits_dateindex ON moz_historyvisits (visit_date);
CREATE UNIQUE INDEX IF NOT EXISTS moz_origins_host_prefix ON moz_origins (host, prefix);
"#;

/// Performance PRAGMAs applied to the destination on open (write throughput;
/// `journal_mode`/`mmap_size` are applied separately because they return values).
const DST_PERF_PRAGMAS: &str = "PRAGMA synchronous = OFF;\
     PRAGMA temp_store = MEMORY;\
     PRAGMA cache_size = -32768;\
     PRAGMA page_size = 4096;";

/// PRAGMAs restored on the destination after migration completes (safe defaults).
const DST_SAFE_PRAGMAS: &str = "PRAGMA synchronous = NORMAL;\
     PRAGMA temp_store = DEFAULT;\
     PRAGMA cache_size = -2000;\
     PRAGMA page_size = 4096;";

/// Transaction wrapper with auto-rollback on drop.
///
/// Internal to `DbContext::with_txn` and `with_dry_run_txn`: it exists so that
/// every exit path from a unit of work (early `?`, dry-run, panic) rolls back
/// unless `commit` ran.
struct AutoRollback<'a> {
    tx: Option<Transaction<'a>>,
}

impl<'a> AutoRollback<'a> {
    fn new(conn: &'a mut Connection) -> Result<Self> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Ok(Self { tx: Some(tx) })
    }

    fn tx(&mut self) -> &mut Transaction<'a> {
        self.tx
            .as_mut()
            .expect("Transaction already committed/rolled back")
    }

    /// Commit and disarm the rollback. Taking the transaction is what disarms
    /// it, so `Drop` has nothing left to undo.
    fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit()?;
        }
        Ok(())
    }
}

impl<'a> Drop for AutoRollback<'a> {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.rollback();
        }
    }
}

/// Row counts for the three migrated tables.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TableCounts {
    pub origins: usize,
    pub places: usize,
    pub visits: usize,
}

/// Thin, intent-revealing wrapper around a SQLite connection.
///
/// This is the seam between the migration orchestration and the database layer:
/// callers open a source (read-only) or destination (read-write) context, ensure
/// the schema, run a unit-of-work inside `with_txn`, and query statistics. All
/// SQLite-specific behavior (connection flags, PRAGMAs, transaction semantics,
/// schema creation) is concentrated here — there is no parallel set of free
/// functions; `DbContext` is the only entry point.
#[derive(Debug)]
pub struct DbContext {
    conn: Connection,
}

impl DbContext {
    /// Open the source database read-only.
    pub fn open_source_db(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, SRC_OPEN_FLAGS)?;
        // Disable foreign key checks to speed up the read-only source.
        conn.execute("PRAGMA foreign_keys = OFF", [])?;
        Ok(Self { conn })
    }

    /// Open the destination database read-write with write optimizations.
    pub fn open_dest_db(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, DST_OPEN_FLAGS)?;
        // Fail fast if SQLite silently downgraded us to a read-only open (its
        // Win32 VFS falls back to READONLY when the read-write CreateFile fails
        // but the file is readable). Without this check the failure resurfaces
        // much later as an opaque "attempt to write a readonly database" on
        // the first write.
        if conn.is_readonly(rusqlite::DatabaseName::Main)? {
            return Err(crate::error::Error::ReadOnlyDestination {
                path: path.to_path_buf(),
            });
        }
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        conn.execute_batch(DST_PERF_PRAGMAS)?;
        let _: Option<String> = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        let _: Option<i64> = conn.query_row("PRAGMA mmap_size = 268435456", [], |r| r.get(0))?;
        Ok(Self { conn })
    }

    /// Ensure the destination schema exists (idempotent).
    pub fn ensure_schema(&self) -> Result<()> {
        self.conn.execute_batch(CREATE_TABLES_SQL)?;
        Ok(())
    }

    /// Run a closure inside an `IMMEDIATE` transaction and commit on `Ok`.
    ///
    /// If the closure returns `Err`, the transaction is rolled back and the
    /// error propagated. This keeps the write unit of work atomic and hidden
    /// behind the seam; callers never reason about commit/rollback ordering.
    pub fn with_txn<F, R>(&mut self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Transaction) -> Result<R>,
    {
        let mut rollback = AutoRollback::new(&mut self.conn)?;
        let value = f(rollback.tx())?;
        rollback.commit()?;
        Ok(value)
    }

    /// Run a closure inside an `IMMEDIATE` transaction and **always** roll
    /// back, returning the closure's value anyway.
    ///
    /// This is how `--dry-run` exercises the real migration code path against
    /// the destination without committing anything.
    pub fn with_dry_run_txn<F, R>(&mut self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Transaction) -> Result<R>,
    {
        let mut rollback = AutoRollback::new(&mut self.conn)?;
        let value = f(rollback.tx())?;
        drop(rollback); // not committed -> drop rolls it back
        Ok(value)
    }

    /// Return the underlying connection for read-only inspection by callers that
    /// must query the raw DB (e.g. validation against both databases).
    pub fn as_conn(&self) -> &Connection {
        &self.conn
    }

    /// Table counts (origins, places, visits).
    pub fn table_counts(&self) -> Result<TableCounts> {
        let origins: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM moz_origins", [], |r| r.get(0))?;
        let places: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM moz_places", [], |r| r.get(0))?;
        let visits: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM moz_historyvisits", [], |r| r.get(0))?;
        Ok(TableCounts {
            origins: origins as usize,
            places: places as usize,
            visits: visits as usize,
        })
    }

    /// Restore safe PRAGMAs on the destination after migration completes.
    pub fn restore_pragmas(&self) -> Result<()> {
        self.conn.execute_batch(DST_SAFE_PRAGMAS)?;
        // Re-enable WAL after switching to safe defaults.
        let _: Option<String> = self
            .conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        Ok(())
    }
}

#[cfg(test)]
mod db_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_dbcontext_open_dest_and_schema() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let ctx = DbContext::open_dest_db(&path).unwrap();
        ctx.ensure_schema().unwrap();
        let counts = ctx.table_counts().unwrap();
        assert_eq!(counts, TableCounts::default());
    }

    #[test]
    fn test_dbcontext_txn_commits_on_ok() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let mut ctx = DbContext::open_dest_db(&path).unwrap();
        ctx.ensure_schema().unwrap();
        ctx.with_txn(|tx| {
            tx.execute(
                "INSERT INTO moz_places (url, title, rev_host, url_hash, guid) VALUES (?, ?, ?, ?, ?)",
                rusqlite::params!["https://example.com/", "Example", "moc.elpmaxe", 1, "abc"],
            )?;
            Ok(())
        })
        .unwrap();
        let counts = ctx.table_counts().unwrap();
        assert_eq!(counts.places, 1);
    }

    #[test]
    fn test_dbcontext_txn_rolls_back_on_err() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let mut ctx = DbContext::open_dest_db(&path).unwrap();
        ctx.ensure_schema().unwrap();
        ctx.with_txn(|tx| {
            tx.execute(
                "INSERT INTO moz_places (url, title, rev_host, url_hash, guid) VALUES (?, ?, ?, ?, ?)",
                rusqlite::params!["https://example.com/", "Example", "moc.elpmaxe", 1, "abc"],
            )?;
            // Force an error to exercise the rollback path.
            tx.execute("INSERT INTO nonexistent_table VALUES (1)", [])?;
            Ok::<(), crate::error::Error>(())
        })
        .unwrap_err();
        let counts = ctx.table_counts().unwrap();
        assert_eq!(counts.places, 0);
    }

    #[test]
    fn test_dbcontext_txn_dry_run_rolls_back() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("test.db");
        let mut ctx = DbContext::open_dest_db(&path).unwrap();
        ctx.ensure_schema().unwrap();
        ctx.with_dry_run_txn(|tx| {
            tx.execute(
                "INSERT INTO moz_places (url, title, rev_host, url_hash, guid) VALUES (?, ?, ?, ?, ?)",
                rusqlite::params!["https://example.com/", "Example", "moc.elpmaxe", 1, "abc"],
            )?;
            Ok(())
        })
        .unwrap();
        let counts = ctx.table_counts().unwrap();
        assert_eq!(counts.places, 0);
    }

    #[test]
    fn test_open_dest_db_rejects_silently_readonly_open() {
        // A read-only file makes SQLite's VFS fall back to a read-only open even
        // though we requested READ_WRITE (silent downgrade). open_dest_db must
        // fail fast with ReadOnlyDestination instead of deferring the failure to
        // the first write ("attempt to write a readonly database").
        //
        // Platforms that ignore the read-only bit (root with CAP_DAC_OVERRIDE
        // opens 0444 files read-write) can never trigger the downgrade, so the
        // rejection assertions are skipped there.
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("ro.db");
        drop(DbContext::open_dest_db(&path).unwrap());

        let orig = std::fs::metadata(&path).unwrap().permissions();
        let mut ro = orig.clone();
        ro.set_readonly(true);
        std::fs::set_permissions(&path, ro).unwrap();

        // Double-confirm the precondition: the platform must actually enforce
        // the read-only bit for a downgrade (and thus a rejection) to happen.
        let bit_enforced = std::fs::OpenOptions::new().write(true).open(&path).is_err();

        let result = DbContext::open_dest_db(&path);

        // Restore writability BEFORE asserting: a failing assertion must not
        // leave a read-only file behind (TempDir::Drop ignores delete errors).
        std::fs::set_permissions(&path, orig).unwrap();

        if bit_enforced {
            // The bit is enforced, so the silent downgrade must have occurred
            // and open_dest_db must have rejected it.
            let err = result
                .expect_err("read-only bit is enforced but open_dest_db did not reject the open");
            assert!(
                matches!(err, crate::error::Error::ReadOnlyDestination { .. }),
                "unexpected error: {err}"
            );
        }
        // else: nothing to assert — the downgrade cannot occur here.
    }

    #[test]
    fn test_dbcontext_open_source_readonly() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("src.db");
        let ctx = DbContext::open_dest_db(&path).unwrap();
        ctx.ensure_schema().unwrap();
        assert_eq!(ctx.table_counts().unwrap(), TableCounts::default());
        drop(ctx);

        // A read-only context must reject writes (BEGIN IMMEDIATE or the
        // write itself fails with SQLite's readonly error).
        let mut ro_ctx = DbContext::open_source_db(&path).unwrap();
        let err = ro_ctx
            .with_txn(|tx| {
                tx.execute(
                    "INSERT INTO moz_places (url, title, rev_host, url_hash, guid) VALUES (?, ?, ?, ?, ?)",
                    rusqlite::params!["https://evil.example/", "X", "moc.live.evil", 2, "x"],
                )?;
                Ok(())
            })
            .unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("readonly"),
            "unexpected error: {err}"
        );
        assert_eq!(ro_ctx.table_counts().unwrap().places, 0);
    }
}
