// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! Core migration logic

use crate::db::{ensure_schema, get_table_counts, open_dest_db, open_source_db};
use crate::dedup::{
    build_origin_map, build_place_map, build_visit_dedup_set, recalc_frecency, update_meta,
    upsert_origin, upsert_place, upsert_visit,
};
use crate::error::{Error, Result};
use crate::models::{MigrationStats, Origin, Place, Visit};
use crate::profile::Profile;
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::{Connection, Transaction};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Maximum rows per transaction (for future batched commit support)
#[allow(dead_code)]
const MAX_TX_ROWS: usize = 100_000;

/// Migration context
pub struct MigrationContext {
    pub src_profile: Profile,
    pub dst_profile: Profile,
    pub dry_run: bool,
    pub skip_confirmation: bool,
    pub cancel_flag: Arc<AtomicBool>,
}

impl MigrationContext {
    pub fn new(src_profile: Profile, dst_profile: Profile, dry_run: bool) -> Self {
        Self {
            src_profile,
            dst_profile,
            dry_run,
            skip_confirmation: false,
            cancel_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Relaxed)
    }
}

/// Execute migration
pub fn migrate(ctx: &MigrationContext) -> Result<MigrationStats> {
    // 1. Check source and destination are not the same
    if ctx.src_profile.path == ctx.dst_profile.path {
        return Err(Error::SameProfile {
            path: ctx.src_profile.path.clone(),
        });
    }

    // 2. Validate profiles
    ctx.src_profile.validate()?;
    ctx.dst_profile.validate()?;

    // 3. Open databases
    let src_conn = open_source_db(&ctx.src_profile.places_sqlite())?;
    let mut dst_conn = open_dest_db(&ctx.dst_profile.places_sqlite())?;

    // 4. Ensure destination schema
    ensure_schema(&dst_conn)?;

    // 5. Display statistics
    let (src_origins, src_places, src_visits) = get_table_counts(&src_conn)?;
    let (dst_origins, dst_places, dst_visits) = get_table_counts(&dst_conn)?;

    println!(
        "Source ({}): origins={}, places={}, visits={}",
        ctx.src_profile.browser.as_str(),
        src_origins,
        src_places,
        src_visits
    );
    println!(
        "Destination ({}): origins={}, places={}, visits={}",
        ctx.dst_profile.browser.as_str(),
        dst_origins,
        dst_places,
        dst_visits
    );

    if ctx.dry_run {
        println!("\n[DRY RUN] Simulation mode, no writes to destination");
    }

    // 6. Confirmation prompt (non dry-run and not skip_confirmation)
    if !ctx.dry_run && !ctx.skip_confirmation {
        print!("\nConfirm start migration? [y/N]: ");
        use std::io::{self, Write};
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            return Err(Error::BackupNotConfirmed);
        }
    }

    // 7. Execute migration
    let start = Instant::now();
    let mut stats = MigrationStats::default();

    // Progress bar
    let total_items = src_origins + src_places + src_visits;
    let pb = ProgressBar::new(total_items as u64);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) {msg}")
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏  "));

    // --- Migrate all data within a single transaction ---
    {
        let mut tx = dst_conn.transaction()?;

        // --- Migrate Origins ---
        pb.set_message("Migrating Origins...");
        stats.origins_read = migrate_origins(&src_conn, &mut tx, &mut stats, &pb, ctx)?;

        if ctx.is_cancelled() {
            return Err(Error::Other("User cancelled".into()));
        }

        // --- Migrate Places ---
        pb.set_message("Migrating Places...");
        stats.places_read = migrate_places(&src_conn, &mut tx, &mut stats, &pb, ctx)?;

        if ctx.is_cancelled() {
            return Err(Error::Other("User cancelled".into()));
        }

        // --- Migrate Visits ---
        pb.set_message("Migrating Visits...");
        stats.visits_read = migrate_visits(&src_conn, &mut tx, &mut stats, &pb, ctx)?;

        // Commit transaction
        if !ctx.dry_run {
            pb.set_message("Committing transaction...");
            tx.commit()?;
        }
    } // Transaction dropped here, dst_conn available again

    // 9. Recalculate frecency (outside transaction)
    if !ctx.dry_run {
        pb.set_message("Recalculating frecency...");
        recalc_frecency(&dst_conn)?;
        // Restore safe PRAGMA
        dst_conn.execute("PRAGMA synchronous = NORMAL", [])?;
        let _: Option<String> =
            dst_conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    }

    // 10. Update metadata
    if !ctx.dry_run {
        update_meta(&dst_conn, &stats)?;
    }

    stats.duration_ms = start.elapsed().as_millis() as u64;
    pb.finish_with_message("Done");

    // 11. Validate
    if !ctx.dry_run {
        validate_migration(&src_conn, &dst_conn, &stats)?;
    }

    Ok(stats)
}

/// Migrate Origins
fn migrate_origins(
    src: &Connection,
    tx: &mut Transaction,
    stats: &mut MigrationStats,
    pb: &ProgressBar,
    ctx: &MigrationContext,
) -> Result<usize> {
    let mut origin_map = build_origin_map(tx)?;
    let mut stmt = src.prepare("SELECT * FROM moz_origins")?;
    let mut rows = stmt.query([])?;
    let mut count = 0;

    while let Some(row) = rows.next()? {
        if ctx.is_cancelled() {
            break;
        }

        let origin = Origin::from_row(row)?;
        let key = origin.unique_key();
        let was_present = origin_map.contains_key(&key);
        let _new_id = upsert_origin(tx, &origin, &mut origin_map)?;

        if was_present {
            stats.origins_merged += 1;
        } else {
            stats.origins_inserted += 1;
        }
        count += 1;
        pb.inc(1);
    }

    Ok(count)
}

/// Migrate Places
fn migrate_places(
    src: &Connection,
    tx: &mut Transaction,
    stats: &mut MigrationStats,
    pb: &ProgressBar,
    ctx: &MigrationContext,
) -> Result<usize> {
    let mut place_map = build_place_map(tx)?;
    let mut origin_map = build_origin_map(tx)?;
    let mut stmt = src.prepare("SELECT * FROM moz_places ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut count = 0;

    while let Some(row) = rows.next()? {
        if ctx.is_cancelled() {
            break;
        }

        let place = Place::from_row(row)?;

        // Ensure origin exists first
        let new_origin_id = if let Some(orig_id) = place.origin_id {
            // Find origin in source database
            let mut origin_stmt = src.prepare("SELECT * FROM moz_origins WHERE id = ?")?;
            if let Ok(orig_row) = origin_stmt.query_row([orig_id], Origin::from_row) {
                Some(upsert_origin(tx, &orig_row, &mut origin_map)?)
            } else {
                None
            }
        } else {
            None
        };

        let (_, merged) = upsert_place(tx, &place, &mut place_map, new_origin_id)?;
        if merged {
            stats.places_merged += 1;
        } else {
            stats.places_inserted += 1;
        }
        count += 1;
        pb.inc(1);
    }

    Ok(count)
}

/// Migrate Visits
fn migrate_visits(
    src: &Connection,
    tx: &mut Transaction,
    stats: &mut MigrationStats,
    pb: &ProgressBar,
    ctx: &MigrationContext,
) -> Result<usize> {
    let mut visit_dedup = build_visit_dedup_set(tx)?;
    let place_map = build_place_map(tx)?;
    let mut stmt = src.prepare("SELECT * FROM moz_historyvisits ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut count = 0;

    while let Some(row) = rows.next()? {
        if ctx.is_cancelled() {
            break;
        }

        let visit = Visit::from_row(row)?;
        let inserted = upsert_visit(tx, &visit, &mut visit_dedup, &place_map)?;

        if inserted {
            stats.visits_inserted += 1;
        } else {
            stats.visits_skipped += 1;
        }
        count += 1;
        pb.inc(1);
    }

    Ok(count)
}

/// Post-migration validation
fn validate_migration(_src: &Connection, dst: &Connection, stats: &MigrationStats) -> Result<()> {
    let (_, dst_places, dst_visits) = get_table_counts(dst)?;

    // Basic integrity check
    if stats.places_inserted + stats.places_merged == 0 && stats.visits_inserted == 0 {
        return Err(Error::ValidationFailed {
            detail: "No data migrated".into(),
        });
    }

    // Check foreign key integrity
    let orphan_visits: i64 = dst.query_row(
        "SELECT COUNT(*) FROM moz_historyvisits v LEFT JOIN moz_places p ON v.place_id = p.id WHERE p.id IS NULL",
        [],
        |r| r.get(0),
    )?;
    if orphan_visits > 0 {
        return Err(Error::ValidationFailed {
            detail: format!("Found {} orphan visits (place_id not found)", orphan_visits),
        });
    }

    println!(
        "\n✅ Validation passed: destination places={}, visits={}",
        dst_places, dst_visits
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_dest_db;
    use crate::profile::Profile;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn create_test_profile(dir: &PathBuf, name: &str) -> Profile {
        Profile {
            name: name.into(),
            path: dir.clone(),
            is_default: true,
            is_relative: true,
            browser: crate::profile::Browser::Firefox,
        }
    }

    #[test]
    fn test_migration_dry_run() {
        let tmp = tempdir().unwrap();
        let src_dir = tmp.path().join("src");
        let dst_dir = tmp.path().join("dst");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Create source database
        let src_db = src_dir.join("places.sqlite");
        let src_conn = open_dest_db(&src_db).unwrap();
        // Execute CREATE TABLE separately to avoid execute_batch issues
        src_conn
            .execute_batch(
                r#"
            CREATE TABLE moz_origins (
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
            CREATE TABLE moz_places (
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
            CREATE TABLE moz_historyvisits (
                id INTEGER PRIMARY KEY,
                from_visit INTEGER,
                place_id INTEGER,
                visit_date INTEGER,
                visit_type INTEGER,
                session INTEGER,
                source INTEGER DEFAULT 0 NOT NULL,
                triggeringPlaceId INTEGER
            );
        "#,
            )
            .unwrap();

        // Insert test data
        src_conn.execute(
            "INSERT INTO moz_origins (prefix, host, frecency) VALUES ('https://', 'example.com', 100)",
            [],
        ).unwrap();
        src_conn.execute(
            "INSERT INTO moz_places (url, title, rev_host, visit_count, guid, url_hash) VALUES ('https://example.com/', 'Test', 'moc.elpmaxe', 5, 'test-guid-1', 12345)",
            [],
        ).unwrap();
        src_conn.execute(
            "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (1, 1234567890000000, 1, 1)",
            [],
        ).unwrap();

        // Create destination database
        let dst_db = dst_dir.join("places.sqlite");
        let _ = open_dest_db(&dst_db).unwrap();

        let src_profile = create_test_profile(&src_dir, "src");
        let dst_profile = create_test_profile(&dst_dir, "dst");

        let ctx = MigrationContext::new(src_profile, dst_profile, true);
        let stats = migrate(&ctx).unwrap();

        assert_eq!(stats.origins_read, 1);
        assert_eq!(stats.places_read, 1);
        assert_eq!(stats.visits_read, 1);
    }
}
