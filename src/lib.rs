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
