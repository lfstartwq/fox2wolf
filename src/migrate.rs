// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! Core migration logic
//!
//! This module orchestrates the migration from Firefox to LibreWolf history.
//! The single entrypoint is [`migrate`], which takes a [`MigrationContext`] —
//! all SQLite-specific details are hidden behind [`DbContext`] and [`DedupContext`].

use crate::db::DbContext;
use crate::dedup::{recalc_frecency, update_meta, DedupContext};
use crate::error::{Error, Result};
use crate::models::{MigrationStats, Origin, Place, Visit};
use crate::profile::Profile;
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::Connection;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Migration context — the interface at the migration seam.
#[derive(Clone)]
pub struct MigrationContext {
    pub src_profile: Profile,
    pub dst_profile: Profile,
    pub dry_run: bool,
    pub skip_confirmation: bool,
    pub cancel_flag: Arc<AtomicBool>,
}

impl MigrationContext {
    /// `yes` skips the confirmation prompt.
    pub fn new(src_profile: Profile, dst_profile: Profile, dry_run: bool, yes: bool) -> Self {
        Self {
            src_profile,
            dst_profile,
            dry_run,
            skip_confirmation: yes,
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

/// Execute the migration — the single public entrypoint.
///
/// Invariants:
/// - Source and destination profiles must differ (same profile is an error)
/// - Both profiles must be valid
/// - dry_run means no writes to destination
/// - skip_confirmation (yes) skips the confirmation prompt
///
/// The context hides all orchestration state behind the db/dedup seams.
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

    // 3. Open databases behind the DbContext seam (source stays open for the
    // counts and the three migration scans; validation below reads only the
    // destination).
    let mut db = DbContext::open_dest_db(&ctx.dst_profile.places_sqlite())?;
    let src_db = DbContext::open_source_db(&ctx.src_profile.places_sqlite())?;

    // 4. Ensure destination schema exists (idempotent)
    db.ensure_schema()?;

    // 5. Compute statistics
    let src_counts = src_db.table_counts()?;
    let (src_origins, src_places, src_visits) =
        (src_counts.origins, src_counts.places, src_counts.visits);
    let dst_counts = db.table_counts()?;
    let (dst_origins, dst_places, dst_visits) =
        (dst_counts.origins, dst_counts.places, dst_counts.visits);

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

    // 7. Progress bar over all rows to migrate
    let pb = ProgressBar::new((src_origins + src_places + src_visits) as u64);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) {msg}")
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏  "));

    // 8. Run migration inside a single transaction behind the DbContext seam;
    // stats are accumulated inside the unit of work. `with_txn` commits it,
    // `with_dry_run_txn` exercises the same code and rolls it back.
    let start_time = Instant::now();
    let unit_of_work = |dst_tx: &mut rusqlite::Transaction| -> Result<MigrationStats> {
        let mut stats = MigrationStats::default();

        // 8a. Build dedup behavior object seeded from destination DB via DedupContext
        let mut dedup = DedupContext::load_from(&mut *dst_tx)?;

        // 8b. Origins phase
        migrate_origins_phase(src_db.as_conn(), &mut dedup, &mut stats, ctx, &pb)?;
        if ctx.is_cancelled() {
            return Err(Error::Other("User cancelled".into()));
        }

        // 8c. Places phase
        migrate_places_phase(src_db.as_conn(), &mut dedup, &mut stats, ctx, &pb)?;
        if ctx.is_cancelled() {
            return Err(Error::Other("User cancelled".into()));
        }

        // 8d. Visits phase
        migrate_visits_phase(src_db.as_conn(), &mut dedup, &mut stats, ctx, &pb)?;

        // 8e. Release the dedup context's borrow of the transaction before the
        // frecency/meta updates below (DbContext::with_txn owns commit/rollback).
        drop(dedup);

        // 8f. Recalculate frecency and update meta
        pb.set_message("Finalizing transaction...");
        recalc_frecency(dst_tx)?;
        update_meta(dst_tx, &stats)?;

        Ok(stats)
    };
    let mut stats = if ctx.dry_run {
        db.with_dry_run_txn(unit_of_work)?
    } else {
        db.with_txn(unit_of_work)?
    };

    // 9. Restore safe PRAGMAs after a real migration (a dry-run never wrote
    // anything, so it has nothing to restore)
    if !ctx.dry_run {
        db.restore_pragmas()?;
    }

    stats.duration_ms = start_time.elapsed().as_millis() as u64;
    pb.finish_with_message("Done");

    // 10. Post-migration validation (destination only; skipped in dry-run)
    if !ctx.dry_run {
        validate_migration(&db, &stats)?;
    }

    Ok(stats)
}

/// Phase 1: stream source origins into the destination via `DedupContext`.
///
/// Reads in `ORDER BY id` so insertion order (and therefore first-wins dedup)
/// is deterministic. Cancellation stops the scan mid-stream; the caller decides
/// whether a partially-read phase is an error.
fn migrate_origins_phase(
    src_conn: &Connection,
    dedup: &mut DedupContext,
    stats: &mut MigrationStats,
    ctx: &MigrationContext,
    pb: &ProgressBar,
) -> Result<()> {
    pb.set_message("Migrating Origins...");
    let mut origin_stmt = src_conn.prepare(
        "SELECT id, prefix, host, frecency, recalc_frecency, alt_frecency, recalc_alt_frecency, block_until_ms, block_pages_until_ms FROM moz_origins ORDER BY id",
    )?;
    let mut origins = origin_stmt.query_map([], Origin::from_row)?;

    while let Some(origin) = origins.next().transpose()? {
        if ctx.is_cancelled() {
            break;
        }
        stats.origins_read += 1;
        let (_new_id, inserted) = dedup.upsert_origin(&origin)?;
        if inserted {
            stats.origins_inserted += 1;
        } else {
            stats.origins_merged += 1;
        }
        pb.inc(1);
    }
    Ok(())
}

/// Phase 2: stream source places into the destination via `DedupContext`.
///
/// `place.origin_id` is the source origin id; `upsert_origin` recorded the
/// translation during the origins phase.
fn migrate_places_phase(
    src_conn: &Connection,
    dedup: &mut DedupContext,
    stats: &mut MigrationStats,
    ctx: &MigrationContext,
    pb: &ProgressBar,
) -> Result<()> {
    pb.set_message("Migrating Places...");
    let mut place_stmt = src_conn.prepare(
        "SELECT id, url, title, rev_host, visit_count, hidden, typed, frecency, last_visit_date, guid, foreign_count, url_hash, description, preview_image_url, site_name, origin_id, recalc_frecency, alt_frecency, recalc_alt_frecency FROM moz_places ORDER BY id",
    )?;
    let mut places = place_stmt.query_map([], Place::from_row)?;

    while let Some(place) = places.next().transpose()? {
        if ctx.is_cancelled() {
            break;
        }
        stats.places_read += 1;
        let (_new_id, is_merged) = dedup.upsert_place(&place)?;
        if is_merged {
            stats.places_merged += 1;
        } else {
            stats.places_inserted += 1;
        }
        pb.inc(1);
    }
    Ok(())
}

/// Phase 3: stream source visits into the destination via `DedupContext`,
/// which rewrites `place_id` to the destination id before dedup.
fn migrate_visits_phase(
    src_conn: &Connection,
    dedup: &mut DedupContext,
    stats: &mut MigrationStats,
    ctx: &MigrationContext,
    pb: &ProgressBar,
) -> Result<()> {
    pb.set_message("Migrating Visits...");
    let mut visit_stmt = src_conn.prepare(
        "SELECT id, from_visit, place_id, visit_date, visit_type, session, source, triggeringPlaceId FROM moz_historyvisits ORDER BY id",
    )?;
    let mut visits = visit_stmt.query_map([], Visit::from_row)?;

    while let Some(visit) = visits.next().transpose()? {
        if ctx.is_cancelled() {
            break;
        }
        stats.visits_read += 1;
        let inserted = dedup.upsert_visit(&visit)?;
        if inserted {
            stats.visits_inserted += 1;
        } else {
            stats.visits_skipped += 1;
        }
        pb.inc(1);
    }
    Ok(())
}

/// Post-migration validation: basic sanity plus foreign-key integrity.
fn validate_migration(db: &DbContext, stats: &MigrationStats) -> Result<()> {
    let dst_counts = db.table_counts()?;
    let (dst_places, dst_visits) = (dst_counts.places, dst_counts.visits);

    if stats.places_inserted + stats.places_merged == 0 && stats.visits_inserted == 0 {
        return Err(Error::ValidationFailed {
            detail: "No data migrated".into(),
        });
    }

    // Check foreign key integrity
    let orphan_visits: i64 = db.as_conn().query_row(
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
    use crate::profile::Browser;
    use std::path::{Path, PathBuf};

    fn create_test_db(dir: &Path) -> PathBuf {
        let db_path = dir.join("places.sqlite");
        let ctx = DbContext::open_dest_db(&db_path).unwrap();
        ctx.ensure_schema().unwrap();
        db_path
    }

    fn insert_test_data(db_path: &Path) {
        let ctx = DbContext::open_dest_db(db_path).unwrap();
        let conn = ctx.as_conn();
        conn.execute(
            "INSERT INTO moz_origins (prefix, host, frecency) VALUES ('https://', 'example.com', 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_origins (prefix, host, frecency) VALUES ('https://', 'test.com', 200)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_places (url, title, rev_host, visit_count, guid, url_hash, origin_id)
             VALUES ('https://example.com/', 'Example', 'moc.elpmaxe', 5, 'guid-1', 111, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_places (url, title, rev_host, visit_count, guid, url_hash, origin_id)
             VALUES ('https://test.com/page', 'Test Page', 'moc.tset', 3, 'guid-2', 222, 2)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (1, 1000000, 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (1, 2000000, 2, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (2, 1500000, 1, 2)",
            [],
        )
        .unwrap();
    }

    fn create_test_profile(dir: &Path, name: &str, browser: Browser) -> Profile {
        Profile {
            name: name.into(),
            path: dir.to_path_buf(),
            is_default: true,
            is_relative: true,
            browser,
        }
    }

    #[test]
    fn test_dry_run_then_real_migration() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("firefox_profile");
        let dst_dir = tmp.path().join("librewolf_profile");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Create source database and insert data
        let src_db = create_test_db(&src_dir);
        insert_test_data(&src_db);

        // Create empty destination database
        let _dst_db = create_test_db(&dst_dir);

        let src_profile = create_test_profile(&src_dir, "firefox-test", Browser::Firefox);
        let dst_profile = create_test_profile(&dst_dir, "librewolf-test", Browser::LibreWolf);

        // Run migration (dry-run)
        let ctx = MigrationContext::new(src_profile.clone(), dst_profile.clone(), true, false);
        let stats = migrate(&ctx).unwrap();

        assert_eq!(stats.origins_read, 2);
        assert_eq!(stats.places_read, 2);
        assert_eq!(stats.visits_read, 3);

        // Actual migration (skip confirmation)
        let ctx = MigrationContext::new(src_profile, dst_profile, false, true);
        let stats = migrate(&ctx).unwrap();

        assert_eq!(stats.origins_read, 2);
        assert_eq!(stats.places_read, 2);
        assert_eq!(stats.visits_read, 3);
        assert_eq!(stats.origins_inserted, 2);
        assert_eq!(stats.places_inserted, 2);
        assert_eq!(stats.visits_inserted, 3);

        // Verify the destination actually holds the expected rows, and that
        // visits point at destination place ids (not copied source ids).
        let dst = DbContext::open_dest_db(&_dst_db).unwrap();
        let counts = dst.table_counts().unwrap();
        assert_eq!(counts.origins, 2);
        assert_eq!(counts.places, 2);
        assert_eq!(counts.visits, 3);

        let orphan_visits: i64 = dst
            .as_conn()
            .query_row(
                "SELECT COUNT(*) FROM moz_historyvisits v
                 LEFT JOIN moz_places p ON v.place_id = p.id
                 WHERE p.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan_visits, 0, "visits must reference existing places");
    }

    /// Regression: source origin ids that are sparse or collide with ids the
    /// destination assigns on its own must be translated to destination ids.
    ///
    /// With contiguous fixture ids (1..N) an untranslated id looks fine, so
    /// this test deliberately gives the source origins ids 50 and 99 — copying
    /// them verbatim would either violate the FK or silently link places to
    /// the wrong origin.
    #[test]
    fn test_sparse_source_origin_ids_are_translated() {
        let tmp = tempfile::tempdir().unwrap();
        let src_dir = tmp.path().join("firefox_profile");
        let dst_dir = tmp.path().join("librewolf_profile");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        let src_db = create_test_db(&src_dir);
        {
            let ctx = DbContext::open_dest_db(&src_db).unwrap();
            let conn = ctx.as_conn();
            conn.execute(
                "INSERT INTO moz_origins (id, prefix, host, frecency) VALUES (50, 'https://', 'sparse.example', 100)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_origins (id, prefix, host, frecency) VALUES (99, 'https://', 'other.example', 200)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_places (url, title, rev_host, visit_count, guid, url_hash, origin_id)
                 VALUES ('https://sparse.example/', 'Sparse', 'elpmaxs.es', 4, 'guid-sparse', 555, 50)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_places (url, title, rev_host, visit_count, guid, url_hash, origin_id)
                 VALUES ('https://other.example/', 'Other', 'rehcto.elpmaxs', 2, 'guid-other', 666, 99)",
                [],
            )
            .unwrap();
        }
        let _dst_db = create_test_db(&dst_dir);

        let ctx = MigrationContext::new(
            create_test_profile(&src_dir, "firefox-sparse", Browser::Firefox),
            create_test_profile(&dst_dir, "librewolf-sparse", Browser::LibreWolf),
            false,
            true,
        );
        let stats = migrate(&ctx).unwrap();

        assert_eq!(stats.origins_inserted, 2);
        assert_eq!(stats.places_inserted, 2);

        // Every destination place must reference an origin row that exists in
        // the destination (i.e. the id was translated, not copied).
        let dst = DbContext::open_dest_db(&_dst_db).unwrap();
        let dangling: i64 = dst
            .as_conn()
            .query_row(
                "SELECT COUNT(*) FROM moz_places p
                 LEFT JOIN moz_origins o ON p.origin_id = o.id
                 WHERE p.origin_id IS NOT NULL AND o.id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            dangling, 0,
            "place.origin_id must point at a destination origin row"
        );

        // The destination assigned its own ids (1, 2); source ids 50/99 must
        // not have been reused.
        let max_origin_id: i64 = dst
            .as_conn()
            .query_row("SELECT MAX(id) FROM moz_origins", [], |r| r.get(0))
            .unwrap();
        assert!(
            max_origin_id < 50,
            "destination must assign fresh ids, got {max_origin_id}"
        );

        // And each place must land on the origin matching its host.
        let hosts_match: i64 = dst
            .as_conn()
            .query_row(
                "SELECT COUNT(*) FROM moz_places p JOIN moz_origins o ON p.origin_id = o.id
                 WHERE (p.url = 'https://sparse.example/' AND o.host = 'sparse.example')
                    OR (p.url = 'https://other.example/' AND o.host = 'other.example')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hosts_match, 2, "each place must keep its own origin");
    }
}
