//! Database connection, Schema constants, transaction management

use crate::error::Result;
use parking_lot::Mutex;
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::path::Path;
use std::sync::Arc;

/// Flags for opening source database read-only
pub const SRC_OPEN_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_ONLY
    .union(OpenFlags::SQLITE_OPEN_NO_MUTEX)
    .union(OpenFlags::SQLITE_OPEN_FULL_MUTEX);

/// Flags for opening destination database read-write
pub const DST_OPEN_FLAGS: OpenFlags = OpenFlags::SQLITE_OPEN_READ_WRITE
    .union(OpenFlags::SQLITE_OPEN_CREATE)
    .union(OpenFlags::SQLITE_OPEN_FULL_MUTEX);

/// Safe PRAGMA settings (after migration completes)
pub const PRAGMA_SAFE: &str = r#"
    PRAGMA synchronous = NORMAL;
    PRAGMA journal_mode = WAL;
"#;

/// Table name constants
pub mod tables {
    pub const MOZ_ORIGINS: &str = "moz_origins";
    pub const MOZ_PLACES: &str = "moz_places";
    pub const MOZ_PLACES_EXTRA: &str = "moz_places_extra";
    pub const MOZ_HISTORYVISITS: &str = "moz_historyvisits";
    pub const MOZ_HISTORYVISITS_EXTRA: &str = "moz_historyvisits_extra";
    pub const MOZ_BOOKMARKS: &str = "moz_bookmarks";
    pub const MOZ_BOOKMARKS_DELETED: &str = "moz_bookmarks_deleted";
    pub const MOZ_KEYWORDS: &str = "moz_keywords";
    pub const MOZ_ANNO_ATTRIBUTES: &str = "moz_anno_attributes";
    pub const MOZ_ANNOS: &str = "moz_annos";
    pub const MOZ_ITEMS_ANNOS: &str = "moz_items_annos";
    pub const MOZ_META: &str = "moz_meta";
    pub const MOZ_PLACES_METADATA: &str = "moz_places_metadata";
    pub const MOZ_PLACES_METADATA_SEARCH_QUERIES: &str = "moz_places_metadata_search_queries";
    pub const MOZ_INPUTHISTORY: &str = "moz_inputhistory";
}

/// SQL for creating tables (core history tables only)
pub const CREATE_TABLES_SQL: &str = r#"
-- moz_origins
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

-- moz_places
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

-- moz_historyvisits
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

/// Open source database read-only
pub fn open_source_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, SRC_OPEN_FLAGS)?;
    // Disable foreign key checks to speed up read-only
    conn.execute("PRAGMA foreign_keys = OFF", [])?;
    Ok(conn)
}

/// Open destination database read-write
pub fn open_dest_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, DST_OPEN_FLAGS)?;
    conn.execute("PRAGMA foreign_keys = ON", [])?;
    // Apply write optimizations - use execute_batch for PRAGMAs that don't return results
    conn.execute_batch(
        "PRAGMA synchronous = OFF;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -32768;
         PRAGMA page_size = 4096;",
    )?;
    // journal_mode and mmap_size may return results, handle separately
    let _: Option<String> = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    let _: Option<i64> = conn.query_row("PRAGMA mmap_size = 268435456", [], |r| r.get(0))?;
    Ok(conn)
}

/// Ensure destination database schema exists
pub fn ensure_schema(conn: &Connection) -> Result<()> {
    // Execute CREATE TABLE statements separately to avoid execute_batch issues
    conn.execute_batch(
        r#"
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
    "#,
    )?;
    // Indexes
    conn.execute_batch(r#"
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
    "#)?;
    Ok(())
}

/// Transaction wrapper with auto-rollback
pub struct AutoRollback<'a> {
    tx: Option<Transaction<'a>>,
    committed: bool,
}

impl<'a> AutoRollback<'a> {
    pub fn new(conn: &'a mut Connection) -> Result<Self> {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Ok(Self {
            tx: Some(tx),
            committed: false,
        })
    }

    pub fn tx(&mut self) -> &mut Transaction<'a> {
        self.tx
            .as_mut()
            .expect("Transaction already committed/rolled back")
    }

    pub fn commit(mut self) -> Result<()> {
        if let Some(tx) = self.tx.take() {
            tx.commit()?;
            self.committed = true;
        }
        Ok(())
    }
}

impl<'a> Drop for AutoRollback<'a> {
    fn drop(&mut self) {
        if !self.committed {
            if let Some(tx) = self.tx.take() {
                let _ = tx.rollback();
            }
        }
    }
}

/// Batch insert helper
pub struct BatchInserter<'a> {
    tx: &'a mut Transaction<'a>,
    sql: String,
    batch_size: usize,
    pending: usize,
}

impl<'a> BatchInserter<'a> {
    pub fn new(tx: &'a mut Transaction<'a>, sql: String, batch_size: usize) -> Self {
        Self {
            tx,
            sql,
            batch_size,
            pending: 0,
        }
    }

    pub fn push<P: rusqlite::Params>(&mut self, params: P) -> Result<()> {
        self.tx.execute(&self.sql, params)?;
        self.pending += 1;
        if self.pending >= self.batch_size {
            self.flush()?;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<()> {
        if self.pending > 0 {
            self.pending = 0;
        }
        Ok(())
    }
}

/// Get database table counts
pub fn get_table_counts(conn: &Connection) -> Result<(usize, usize, usize)> {
    let origins: i64 = conn.query_row("SELECT COUNT(*) FROM moz_origins", [], |r| r.get(0))?;
    let places: i64 = conn.query_row("SELECT COUNT(*) FROM moz_places", [], |r| r.get(0))?;
    let visits: i64 = conn.query_row("SELECT COUNT(*) FROM moz_historyvisits", [], |r| r.get(0))?;
    Ok((origins as usize, places as usize, visits as usize))
}

/// Check schema version compatibility
pub fn check_schema_version(conn: &Connection) -> Result<i32> {
    let version: i32 = conn
        .query_row(
            "SELECT value FROM moz_meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    Ok(version)
}

/// Thread-safe database connection pool (simple version)
pub type SharedConnection = Arc<Mutex<Connection>>;

pub fn new_shared_connection(path: &Path, read_only: bool) -> Result<SharedConnection> {
    let flags = if read_only {
        SRC_OPEN_FLAGS
    } else {
        DST_OPEN_FLAGS
    };
    let conn = Connection::open_with_flags(path, flags)?;
    if !read_only {
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        conn.execute_batch(
            "PRAGMA synchronous = OFF;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -32768;
             PRAGMA page_size = 4096;",
        )?;
        let _: Option<String> = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        let _: Option<i64> = conn.query_row("PRAGMA mmap_size = 268435456", [], |r| r.get(0))?;
    }
    Ok(Arc::new(Mutex::new(conn)))
}
