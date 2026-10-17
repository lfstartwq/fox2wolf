// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! fox2wolf - Firefox to LibreWolf history migration tool
//!
//! Migrates Firefox places.sqlite history to LibreWolf,
//! supporting merge deduplication and single-transaction processing for large datasets.

pub mod db;
pub mod dedup;
pub mod error;
pub mod migrate;
pub mod models;
pub mod profile;

pub use migrate::{migrate, MigrationContext};
pub use models::MigrationStats;
pub use profile::{discover_profiles, Browser, Profile};

/// Test helper: temp dirs live under `target/` instead of `%TEMP%`, so the
/// suite also runs in environments where `%TEMP%` is not writable.
#[cfg(test)]
pub(crate) mod testutil {
    /// Create a unique temp dir under `target/test-tmp` (cwd is the package root).
    pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
        let base = std::path::Path::new("target").join("test-tmp");
        std::fs::create_dir_all(&base)?;
        tempfile::Builder::new()
            .prefix("fox2wolf-test")
            .tempdir_in(&base)
    }
}
