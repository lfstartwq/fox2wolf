// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! Integration tests

use fox2wolf::db::DbContext;
use fox2wolf::{discover_profiles, migrate, Browser, MigrationContext, MigrationStats, Profile};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn create_test_db(dir: &Path) -> PathBuf {
    let db_path = dir.join("places.sqlite");
    let ctx = DbContext::open_dest_db(&db_path).unwrap();
    ctx.ensure_schema().unwrap();
    db_path
}

fn insert_test_data(db_path: &Path) {
    let ctx = DbContext::open_dest_db(db_path).unwrap();
    let conn = ctx.as_conn();

    // origins
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

    // places
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

    // visits
    conn.execute(
        "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (1, 1000000, 1, 1)",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (1, 2000000, 2, 1)",
        [],  // typed visit
    ).unwrap();
    conn.execute(
        "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (2, 1500000, 1, 2)",
        [],
    ).unwrap();
}

fn create_profile(dir: &Path, name: &str, browser: Browser) -> Profile {
    Profile {
        name: name.into(),
        path: dir.to_path_buf(),
        is_default: true,
        is_relative: true,
        browser,
    }
}

#[test]
fn test_full_migration() {
    let tmp = tempdir().unwrap();
    let src_dir = tmp.path().join("firefox_profile");
    let dst_dir = tmp.path().join("librewolf_profile");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();

    // Create source database and insert data
    let src_db = create_test_db(&src_dir);
    insert_test_data(&src_db);

    // Create empty destination database
    let dst_db = create_test_db(&dst_dir);

    let src_profile = create_profile(&src_dir, "firefox-test", Browser::Firefox);
    let dst_profile = create_profile(&dst_dir, "librewolf-test", Browser::LibreWolf);

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

    // Verify destination database
    let db = DbContext::open_dest_db(&dst_db).unwrap();
    let counts = db.table_counts().unwrap();
    assert_eq!(counts.origins, 2);
    assert_eq!(counts.places, 2);
    assert_eq!(counts.visits, 3);
}

#[test]
fn test_merge_deduplication() {
    let tmp = tempdir().unwrap();
    let src_dir = tmp.path().join("firefox_profile");
    let dst_dir = tmp.path().join("librewolf_profile");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&dst_dir).unwrap();

    // Pre-populate destination with same URL
    let dst_db = create_test_db(&dst_dir);
    {
        let ctx = DbContext::open_dest_db(&dst_db).unwrap();
        let conn = ctx.as_conn();
        conn.execute(
            "INSERT INTO moz_origins (prefix, host, frecency) VALUES ('https://', 'example.com', 50)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO moz_places (url, title, rev_host, visit_count, guid, url_hash, origin_id) 
             VALUES ('https://example.com/', 'Existing', 'moc.elpmaxe', 10, 'existing-guid', 111, 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (1, 500000, 1, 1)",
            [],
        ).unwrap();
    }

    // Source has same URL, different visit_count
    let src_db = create_test_db(&src_dir);
    {
        let ctx = DbContext::open_dest_db(&src_db).unwrap();
        let conn = ctx.as_conn();
        conn.execute(
            "INSERT INTO moz_origins (prefix, host, frecency) VALUES ('https://', 'example.com', 100)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO moz_places (url, title, rev_host, visit_count, guid, url_hash, origin_id) 
             VALUES ('https://example.com/', 'From Firefox', 'moc.elpmaxe', 5, 'src-guid', 111, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_historyvisits (place_id, visit_date, visit_type, session) VALUES (1, 3000000, 2, 1)",
            [],
        ).unwrap();
    }

    let src_profile = create_profile(&src_dir, "firefox", Browser::Firefox);
    let dst_profile = create_profile(&dst_dir, "librewolf", Browser::LibreWolf);

    let ctx = MigrationContext::new(src_profile, dst_profile, false, true);
    let stats = migrate(&ctx).unwrap();

    // Verify merge results
    assert_eq!(stats.places_merged, 1); // 1 merged
    assert_eq!(stats.places_inserted, 0); // 0 inserted
    assert_eq!(stats.visits_inserted, 1); // 1 new visit

    let db = DbContext::open_dest_db(&dst_db).unwrap();
    let visit_count: i64 = db
        .as_conn()
        .query_row(
            "SELECT visit_count FROM moz_places WHERE url = 'https://example.com/'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Destination had 10 + source 5 = 15
    assert_eq!(visit_count, 15);

    let last_visit: Option<i64> = db
        .as_conn()
        .query_row(
            "SELECT last_visit_date FROM moz_places WHERE url = 'https://example.com/'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Source place last_visit_date is NULL (not set in test), destination also NULL, merged result is NULL
    // In real scenarios Firefox maintains this field, here we just verify merge logic doesn't error
    assert_eq!(last_visit, None);
}

#[test]
fn test_discover_profiles() {
    // Just test no panic, depends on actual environment
    let _ = discover_profiles(Browser::Firefox);
    let _ = discover_profiles(Browser::LibreWolf);
}

#[test]
fn test_migration_stats_display() {
    let stats = MigrationStats {
        origins_read: 10,
        origins_inserted: 8,
        origins_merged: 2,
        places_read: 100,
        places_inserted: 90,
        places_merged: 10,
        places_skipped: 0,
        visits_read: 500,
        visits_inserted: 480,
        visits_skipped: 20,
        duration_ms: 1234,
    };

    stats.print_summary(); // Just verify no panic
}
