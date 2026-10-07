// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! Deduplication and merge algorithms

use crate::error::Result;
use crate::models::{Origin, Place, Visit};
use rusqlite::{Connection, Transaction};
use std::collections::{HashMap, HashSet};

/// Origin dedup map: (host, prefix) -> (old_id, new_id)
pub type OriginMap = HashMap<String, (i64, i64)>;

/// Place dedup map: dedup_key -> (old_id, new_id)
pub type PlaceMap = HashMap<String, (i64, i64)>;

/// Visit dedup set: dedup_key
pub type VisitDedupSet = HashSet<String>;

/// Behavior object hiding map plumbing behind `upsert_*` methods.
/// Orchestration sees upsert behavior, not map manipulation.
///
/// Invariants:
/// - Maps are seeded from destination DB via `load_from`
/// - The transaction is **borrowed** from `DbContext::with_txn` for the
///   duration of the migration unit; the caller owns commit/rollback
pub struct DedupContext<'a, 'c> {
    origin_map: OriginMap,
    place_map: PlaceMap,
    visit_dedup: VisitDedupSet,
    /// Source place id -> destination place id for this migration run.
    /// Seeded only by `upsert_place` (not from the destination DB), so visits
    /// can be rewritten to destination ids just like the old `place_id_map`.
    place_id_map: HashMap<i64, i64>,
    /// Source origin id -> destination origin id for this migration run.
    /// Recorded by `upsert_origin` so `upsert_place` can translate the source
    /// `moz_places.origin_id` into a valid destination id (the old code did
    /// this per-place by re-reading the source origin row).
    origin_id_map: HashMap<i64, i64>,
    tx: &'a mut Transaction<'c>,
}

impl<'a, 'c> DedupContext<'a, 'c> {
    /// Load dedup maps from the destination database and bind to a transaction.
    ///
    /// Seeds the internal OriginMap, PlaceMap, and VisitDedupSet by reading
    /// existing rows from the database, then binds to the provided transaction
    /// for upsert operations.
    pub fn load_from(tx: &'a mut Transaction<'c>) -> Result<Self> {
        let (map, place_map, visit_dedup) = {
            let conn: &Connection = &*tx; // Deref Transaction to its Connection

            let mut map = HashMap::new();
            // Explicit column lists in from_row's positional order: `SELECT *`
            // would silently mis-key the maps if a schema ever reordered columns.
            let mut stmt = conn.prepare(
                "SELECT id, prefix, host, frecency, recalc_frecency, alt_frecency, recalc_alt_frecency, block_until_ms, block_pages_until_ms FROM moz_origins",
            )?;
            let rows = stmt.query_map([], Origin::from_row)?;
            for r in rows {
                let origin = r?;
                // Key comes from Origin::unique_key so seeding and upsert can
                // never drift apart.
                map.insert(origin.unique_key(), (origin.id, origin.id)); // (old_id, new_id) - initially same
            }

            let mut place_map = HashMap::new();
            let mut stmt = conn.prepare(
                "SELECT id, url, title, rev_host, visit_count, hidden, typed, frecency, last_visit_date, guid, foreign_count, url_hash, description, preview_image_url, site_name, origin_id, recalc_frecency, alt_frecency, recalc_alt_frecency FROM moz_places",
            )?;
            let rows = stmt.query_map([], Place::from_row)?;
            for r in rows {
                let place = r?;
                place_map.insert(place.dedup_key(), (place.id, place.id));
            }

            let mut visit_dedup = HashSet::new();
            let mut stmt =
                conn.prepare("SELECT place_id, visit_date, visit_type FROM moz_historyvisits")?;
            let rows = stmt.query_map([], |row| {
                let place_id: i64 = row.get(0)?;
                let visit_date: i64 = row.get(1)?;
                let visit_type: i32 = row.get(2)?;
                Ok(Visit::key_of(place_id, visit_date, visit_type))
            })?;
            for r in rows {
                visit_dedup.insert(r?);
            }

            (map, place_map, visit_dedup)
        };

        Ok(Self {
            origin_map: map,
            place_map,
            visit_dedup,
            place_id_map: HashMap::new(),
            origin_id_map: HashMap::new(),
            tx,
        })
    }

    /// Insert or get Origin, return `(new_id, was_inserted)`.
    ///
    /// Checks internal OriginMap first; if present, returns the existing new_id
    /// with `was_inserted = false`. Otherwise, INSERTs into the database via the
    /// bound transaction and returns `was_inserted = true`.
    pub fn upsert_origin(&mut self, origin: &Origin) -> Result<(i64, bool)> {
        let key = origin.unique_key();

        if let Some(&(_, new_id)) = self.origin_map.get(&key) {
            // Source origin id already resolved to an existing destination id.
            self.origin_id_map.insert(origin.id, new_id);
            return Ok((new_id, false));
        }

        // Insert new Origin via transaction
        self.tx.execute(
            "INSERT INTO moz_origins (prefix, host, frecency, recalc_frecency, alt_frecency, recalc_alt_frecency, block_until_ms, block_pages_until_ms)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                origin.prefix,
                origin.host,
                origin.frecency,
                origin.recalc_frecency,
                origin.alt_frecency,
                origin.recalc_alt_frecency,
                origin.block_until_ms,
                origin.block_pages_until_ms,
            ],
        )?;

        let new_id = self.tx.last_insert_rowid();
        self.origin_map.insert(key, (origin.id, new_id));
        self.origin_id_map.insert(origin.id, new_id);
        Ok((new_id, true))
    }

    /// Insert or merge Place, return (new_id, is_merged).
    ///
    /// `place.origin_id` (the **source** `moz_origins.id`) is translated to
    /// the destination id through `origin_id_map` (recorded by `upsert_origin`
    /// during the origins phase) before being written, so a destination
    /// `moz_places.origin_id` never *reuses* a source id that was migrated —
    /// an unmapped (dangling) source id falls through unchanged, matching the
    /// old per-place lookup.
    ///
    /// Checks internal PlaceMap by dedup_key. If present, UPDATEs the database
    /// and returns (existing_id, true). Otherwise, INSERTs with new GUID and
    /// url_hash, returns (new_id, false).
    pub fn upsert_place(&mut self, place: &Place) -> Result<(i64, bool)> {
        // Translate source origin id -> destination origin id (None when the
        // source origin was never migrated, matching the old per-place lookup).
        let new_origin_id = place
            .origin_id
            .and_then(|src_id| self.origin_id_map.get(&src_id).copied());
        let key = place.dedup_key();
        let existing_id = self.place_map.get(&key).map(|&(_, id)| id);

        if let Some(existing_id) = existing_id {
            // Already exists, merge
            let mut stmt = self.tx.prepare(
                "UPDATE moz_places SET
                    visit_count = visit_count + ?,
                    hidden = hidden OR ?,
                    typed = MAX(typed, ?),
                    foreign_count = foreign_count + ?,
                    last_visit_date = CASE
                        WHEN last_visit_date IS NULL THEN ?
                        WHEN ? IS NULL THEN last_visit_date
                        ELSE MAX(last_visit_date, ?)
                    END,
                    origin_id = COALESCE(?, origin_id),
                    recalc_frecency = 1
                WHERE id = ?",
            )?;
            stmt.execute(rusqlite::params![
                place.visit_count,
                place.hidden,
                place.typed,
                place.foreign_count,
                place.last_visit_date,
                place.last_visit_date,
                place.last_visit_date,
                new_origin_id,
                existing_id,
            ])?;
            self.place_id_map.insert(place.id, existing_id);
            Ok((existing_id, true))
        } else {
            // New insert
            let mut place_copy = place.clone();
            place_copy.origin_id = new_origin_id.or(place_copy.origin_id);
            // Generate new GUID
            place_copy.guid = uuid::Uuid::new_v4().to_string();
            // Recalculate url_hash (see compute_url_hash's doc: NOT a bare CRC32)
            place_copy.url_hash = compute_url_hash(&place_copy.url);

            self.tx.execute(
                "INSERT INTO moz_places (url, title, rev_host, visit_count, hidden, typed, frecency, last_visit_date, guid, foreign_count, url_hash, description, preview_image_url, site_name, origin_id, recalc_frecency, alt_frecency, recalc_alt_frecency)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    place_copy.url,
                    place_copy.title,
                    place_copy.rev_host,
                    place_copy.visit_count,
                    place_copy.hidden,
                    place_copy.typed,
                    place_copy.frecency,
                    place_copy.last_visit_date,
                    place_copy.guid,
                    place_copy.foreign_count,
                    place_copy.url_hash,
                    place_copy.description,
                    place_copy.preview_image_url,
                    place_copy.site_name,
                    place_copy.origin_id,
                    place_copy.recalc_frecency,
                    place_copy.alt_frecency,
                    place_copy.recalc_alt_frecency,
                ],
            )?;

            let new_id = self.tx.last_insert_rowid();
            self.place_map.insert(key, (place.id, new_id));
            self.place_id_map.insert(place.id, new_id);
            Ok((new_id, false))
        }
    }

    /// Insert Visit (dedup), rewriting `place_id` to the destination id first.
    ///
    /// Checks internal VisitDedupSet by dedup_key (computed after the rewrite).
    /// If already present, returns false (skip). Otherwise, INSERTs into the
    /// database and inserts key into the set, returns true.
    pub fn upsert_visit(&mut self, visit: &Visit) -> Result<bool> {
        // Rewrite source place_id -> destination place_id (place_id_map is
        // populated by upsert_place during this run; missing keys fall back to
        // the original id, matching the old migrate_visits behavior).
        let place_id = self
            .place_id_map
            .get(&visit.place_id)
            .copied()
            .unwrap_or(visit.place_id);
        let mut visit = visit.clone();
        visit.place_id = place_id;

        let key = visit.dedup_key();

        if self.visit_dedup.contains(&key) {
            return Ok(false); // Already exists, skip
        }

        self.tx.execute(
            "INSERT INTO moz_historyvisits (from_visit, place_id, visit_date, visit_type, session, source, triggeringPlaceId)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                visit.from_visit,
                visit.place_id,
                visit.visit_date,
                visit.visit_type.as_i32(),
                visit.session,
                visit.source,
                visit.triggering_place_id,
            ],
        )?;

        self.visit_dedup.insert(key);
        Ok(true)
    }
}

/// Compute Firefox-style url_hash (CRC32)
///
/// Note: hashes the `str` via `std::hash::Hash`, which suffixes the bytes
/// before CRC32-ing them — so this is not a bare CRC32 of the URL bytes. The
/// exact value is locked by `test_compute_url_hash_matches_pinned_value` and
/// must not change silently: `moz_places.url_hash` is part of the dedup key.
fn compute_url_hash(url: &str) -> i64 {
    use std::hash::Hash;
    let mut hasher = crc32fast::Hasher::new();
    url.hash(&mut hasher);
    hasher.finalize() as i64
}

/// Batch recalculate frecency (call after migration completes)
pub fn recalc_frecency(conn: &Connection) -> Result<()> {
    // Simplified Firefox frecency algorithm: visit_count * 1000 / (days_since_last_visit + 1)
    // Using SQLite built-in time functions
    conn.execute(
        r#"
        UPDATE moz_places SET
            frecency = CASE
                WHEN last_visit_date IS NULL THEN -1
                WHEN visit_count = 0 THEN -1
                ELSE CAST(visit_count * 1000.0 / ((strftime('%s', 'now') * 1000000 - last_visit_date) / 86400000000.0 + 1) AS INTEGER)
            END,
            recalc_frecency = 0
        WHERE recalc_frecency = 1
        "#,
        [],
    )?;
    Ok(())
}

/// Update moz_meta statistics
pub fn update_meta(conn: &Connection, stats: &crate::models::MigrationStats) -> Result<()> {
    let now = chrono::Utc::now().timestamp_micros();
    conn.execute(
        "INSERT OR REPLACE INTO moz_meta (key, value) VALUES (?, ?)",
        rusqlite::params!["last_migration", now.to_string()],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO moz_meta (key, value) VALUES (?, ?)",
        rusqlite::params!["migration_stats", serde_json::to_string(stats)?],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_url_hash() {
        let h1 = compute_url_hash("https://example.com/");
        let h2 = compute_url_hash("https://example.com/");
        assert_eq!(h1, h2);

        let h3 = compute_url_hash("https://other.com/");
        assert_ne!(h1, h3);
    }

    /// Known-vector lock for `compute_url_hash`.
    ///
    /// `moz_places.url_hash` feeds the place dedup key, so a silent change
    /// here would silently change which rows merge. 2127969085 is the value
    /// produced by CRC32 over `str::hash`'s encoding of "https://example.com/".
    /// If this test fails, url_hash generation changed — that is a migration
    /// behavior change, not a test to update casually.
    #[test]
    fn test_compute_url_hash_matches_pinned_value() {
        assert_eq!(compute_url_hash("https://example.com/"), 2127969085);
    }

    /// Build an in-memory destination with the real schema for seam tests.
    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::db::CREATE_TABLES_SQL).unwrap();
        conn.execute("PRAGMA foreign_keys = ON", []).unwrap();
        conn
    }

    #[test]
    fn test_upsert_origin_inserts_then_reuses_and_records_translation() {
        let mut conn = test_conn();
        let mut tx = conn.transaction().unwrap();
        let mut dedup = DedupContext::load_from(&mut tx).unwrap();

        // First sighting of the source origin: inserts a fresh destination row.
        let origin = Origin {
            id: 50,
            prefix: "https://".into(),
            host: "example.com".into(),
            frecency: 100,
            recalc_frecency: 0,
            alt_frecency: None,
            recalc_alt_frecency: 0,
            block_until_ms: None,
            block_pages_until_ms: None,
        };
        let (dest_id, inserted) = dedup.upsert_origin(&origin).unwrap();
        assert!(inserted);
        assert_eq!(dest_id, 1, "first destination origin gets id 1");
        assert_eq!(
            dedup.origin_id_map.get(&50),
            Some(&1),
            "source id 50 must map to destination id 1"
        );

        // A different source row with the same unique key reuses it.
        let dup = Origin {
            id: 99,
            ..origin.clone()
        };
        let (dest_id2, inserted2) = dedup.upsert_origin(&dup).unwrap();
        assert!(!inserted2, "same (prefix, host) must merge, not insert");
        assert_eq!(dest_id2, dest_id);
        assert_eq!(
            dedup.origin_id_map.get(&99),
            Some(&dest_id),
            "colliding source id must map to the same destination origin"
        );
    }

    #[test]
    fn test_upsert_place_translates_source_origin_id() {
        let mut conn = test_conn();
        let mut tx = conn.transaction().unwrap();
        let mut dedup = DedupContext::load_from(&mut tx).unwrap();

        let origin = Origin {
            id: 50,
            prefix: "https://".into(),
            host: "example.com".into(),
            frecency: 100,
            recalc_frecency: 0,
            alt_frecency: None,
            recalc_alt_frecency: 0,
            block_until_ms: None,
            block_pages_until_ms: None,
        };
        dedup.upsert_origin(&origin).unwrap();

        let place = Place {
            id: 7,
            url: "https://example.com/".into(),
            title: Some("Example".into()),
            rev_host: "moc.elpmaxe".into(),
            visit_count: 5,
            hidden: false,
            typed: 0,
            frecency: 100,
            last_visit_date: Some(1_000_000),
            guid: "src-guid".into(),
            foreign_count: 0,
            url_hash: 111,
            description: None,
            preview_image_url: None,
            site_name: None,
            origin_id: Some(50),
            recalc_frecency: 1,
            alt_frecency: None,
            recalc_alt_frecency: 0,
        };
        let (dest_place_id, merged) = dedup.upsert_place(&place).unwrap();
        assert!(!merged);
        assert_eq!(dest_place_id, 1);

        // The run's place_id_map must know source 7 -> destination 1 for visits.
        assert_eq!(dedup.place_id_map.get(&7), Some(&1));
        drop(dedup); // release the transaction before reading it directly

        // The stored origin_id must be the destination id (1), not source id 50.
        let stored_origin: Option<i64> = tx
            .query_row("SELECT origin_id FROM moz_places WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored_origin, Some(1), "origin_id must be translated");
    }

    #[test]
    fn test_upsert_visit_rewrites_place_id_and_skips_duplicates() {
        let mut conn = test_conn();
        let mut tx = conn.transaction().unwrap();
        let mut dedup = DedupContext::load_from(&mut tx).unwrap();

        let place = Place {
            id: 7,
            url: "https://example.com/".into(),
            title: Some("Example".into()),
            rev_host: "moc.elpmaxe".into(),
            visit_count: 1,
            hidden: false,
            typed: 0,
            frecency: 0,
            last_visit_date: None,
            guid: "src-guid".into(),
            foreign_count: 0,
            url_hash: 111,
            description: None,
            preview_image_url: None,
            site_name: None,
            origin_id: None,
            recalc_frecency: 1,
            alt_frecency: None,
            recalc_alt_frecency: 0,
        };
        dedup.upsert_place(&place).unwrap();

        let visit = Visit {
            id: 3,
            from_visit: None,
            place_id: 7, // source place id
            visit_date: 1_000_000,
            visit_type: crate::models::VisitType::LINK,
            session: 1,
            source: 0,
            triggering_place_id: None,
        };
        assert!(dedup.upsert_visit(&visit).unwrap(), "first visit inserts");
        // Same visit again: the key is computed after the place_id rewrite, so
        // it matches and must be skipped rather than duplicated.
        assert!(!dedup.upsert_visit(&visit).unwrap(), "duplicate skipped");
        drop(dedup); // release the transaction before reading it directly

        let stored_place: i64 = tx
            .query_row("SELECT place_id FROM moz_historyvisits", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            stored_place, 1,
            "visit must reference the destination place id, not source id 7"
        );

        let n: i64 = tx
            .query_row("SELECT COUNT(*) FROM moz_historyvisits", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn test_load_from_seeds_maps_from_existing_destination_rows() {
        let mut conn = test_conn();
        conn.execute(
            "INSERT INTO moz_origins (id, prefix, host, frecency) VALUES (3, 'https://', 'seeded.example', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_places (id, url, title, rev_host, visit_count, guid, url_hash)
             VALUES (4, 'https://seeded.example/', 'Seeded', 'elpmaxd.dees', 1, 'seeded-guid', 12345)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_historyvisits (id, place_id, visit_date, visit_type) VALUES (6, 4, 1000000, 1)",
            [],
        )
        .unwrap();

        let mut tx = conn.transaction().unwrap();
        let dedup = DedupContext::load_from(&mut tx).unwrap();

        let origin = Origin {
            id: 999, // different source id, same (prefix, host)
            prefix: "https://".into(),
            host: "seeded.example".into(),
            frecency: 10,
            recalc_frecency: 0,
            alt_frecency: None,
            recalc_alt_frecency: 0,
            block_until_ms: None,
            block_pages_until_ms: None,
        };
        assert!(dedup.origin_map.contains_key(&origin.unique_key()));

        let place = Place {
            id: 42,
            url: "https://seeded.example/".into(),
            title: Some("Seeded".into()),
            rev_host: "elpmaxd.dees".into(),
            visit_count: 1,
            hidden: false,
            typed: 0,
            frecency: 0,
            last_visit_date: None,
            guid: "other-guid".into(),
            foreign_count: 0,
            url_hash: 12345,
            description: None,
            preview_image_url: None,
            site_name: None,
            origin_id: Some(3),
            recalc_frecency: 1,
            alt_frecency: None,
            recalc_alt_frecency: 0,
        };
        assert!(
            dedup.place_map.contains_key(&place.dedup_key()),
            "existing destination place must be seeded so re-runs merge"
        );

        assert!(dedup.visit_dedup.contains(&crate::models::Visit::key_of(
            4,
            1_000_000,
            crate::models::VisitType::LINK.as_i32()
        )));
    }
}
