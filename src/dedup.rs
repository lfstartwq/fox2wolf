//! Deduplication and merge algorithms

use crate::error::Result;
use crate::models::{Origin, Place, Visit};
use rusqlite::{Connection, Transaction};
use std::collections::HashMap;

/// Origin dedup map: (host, prefix) -> (old_id, new_id)
pub type OriginMap = HashMap<String, (i64, i64)>;

/// Place dedup map: dedup_key -> (old_id, new_id)
pub type PlaceMap = HashMap<String, (i64, i64)>;

/// Visit dedup set: dedup_key
pub type VisitDedupSet = std::collections::HashSet<String>;

/// Build existing Origin map from destination database
pub fn build_origin_map(conn: &Connection) -> Result<OriginMap> {
    let mut map = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, prefix, host FROM moz_origins")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let prefix: String = row.get(1)?;
        let host: String = row.get(2)?;
        let key = format!("{} {}", prefix, host);
        Ok((key, id))
    })?;
    for r in rows {
        let (key, id) = r?;
        map.insert(key, (id, id)); // Destination already has it, old_id == new_id
    }
    Ok(map)
}

/// Build existing Place map from destination database (by url_hash + url)
pub fn build_place_map(conn: &Connection) -> Result<PlaceMap> {
    let mut map = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, url_hash, url FROM moz_places")?;
    let rows = stmt.query_map([], |row| {
        let id: i64 = row.get(0)?;
        let url_hash: i64 = row.get(1)?;
        let url: String = row.get(2)?;
        let key = format!("{}:{}", url_hash, url);
        Ok((key, id))
    })?;
    for r in rows {
        let (key, id) = r?;
        map.insert(key, (id, id));
    }
    Ok(map)
}

/// Build existing Visit dedup key set from destination database
pub fn build_visit_dedup_set(conn: &Connection) -> Result<VisitDedupSet> {
    let mut set = VisitDedupSet::new();
    let mut stmt =
        conn.prepare("SELECT place_id, visit_date, visit_type FROM moz_historyvisits")?;
    let rows = stmt.query_map([], |row| {
        let place_id: i64 = row.get(0)?;
        let visit_date: i64 = row.get(1)?;
        let visit_type: i32 = row.get(2)?;
        let key = format!("{}:{}:{}", place_id, visit_date, visit_type);
        Ok(key)
    })?;
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// Insert or get Origin, return new_id
pub fn upsert_origin(
    tx: &mut Transaction,
    origin: &Origin,
    origin_map: &mut OriginMap,
) -> Result<i64> {
    let key = origin.unique_key();

    if let Some(&(_, new_id)) = origin_map.get(&key) {
        return Ok(new_id);
    }

    // Insert new Origin
    tx.execute(
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

    let new_id = tx.last_insert_rowid();
    origin_map.insert(key, (origin.id, new_id));
    Ok(new_id)
}

/// Insert or merge Place, return (new_id, is_merged)
pub fn upsert_place(
    tx: &mut Transaction,
    place: &Place,
    place_map: &mut PlaceMap,
    new_origin_id: Option<i64>,
) -> Result<(i64, bool)> {
    let key = place.dedup_key();

    if let Some(&(_, existing_id)) = place_map.get(&key) {
        // Already exists, merge
        let mut stmt = tx.prepare(
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
        Ok((existing_id, true))
    } else {
        // New insert
        let mut place_copy = place.clone();
        place_copy.origin_id = new_origin_id.or(place_copy.origin_id);
        // Generate new GUID
        place_copy.guid = uuid::Uuid::new_v4().to_string();
        // Recalculate url_hash (Firefox algorithm: CRC32 of url)
        place_copy.url_hash = compute_url_hash(&place_copy.url);

        tx.execute(
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

        let new_id = tx.last_insert_rowid();
        place_map.insert(key, (place.id, new_id));
        Ok((new_id, false))
    }
}

/// Compute Firefox-style url_hash (CRC32)
fn compute_url_hash(url: &str) -> i64 {
    use std::hash::Hash;
    let mut hasher = crc32fast::Hasher::new();
    url.hash(&mut hasher);
    hasher.finalize() as i64
}

/// Insert Visit (dedup)
pub fn upsert_visit(
    tx: &mut Transaction,
    visit: &Visit,
    visit_dedup: &mut VisitDedupSet,
    place_id_map: &PlaceMap,
) -> Result<bool> {
    let key = visit.dedup_key();

    if visit_dedup.contains(&key) {
        return Ok(false); // Already exists, skip
    }

    // Rewrite place_id
    let place_id_str = key.split(':').next().unwrap_or("");
    let new_place_id = place_id_map
        .get(place_id_str)
        .map(|(_, new_id)| *new_id)
        .unwrap_or(visit.place_id);

    tx.execute(
        "INSERT INTO moz_historyvisits (from_visit, place_id, visit_date, visit_type, session, source, triggeringPlaceId)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            visit.from_visit,
            new_place_id,
            visit.visit_date,
            visit.visit_type.as_i32(),
            visit.session,
            visit.source,
            visit.triggering_place_id,
        ],
    )?;

    visit_dedup.insert(key);
    Ok(true)
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
}
