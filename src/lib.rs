// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! fox2wolf - Firefox to LibreWolf history migration tool
//!
//! Migrates Firefox places.sqlite history to LibreWolf,
//! supporting merge deduplication and batch processing for large datasets.

pub mod db;
pub mod dedup;
pub mod error;
pub mod migrate;
pub mod models;
pub mod profile;

pub use error::{Error, Result};
pub use migrate::{migrate, MigrationContext};
pub use models::{Microseconds, MigrationStats, Origin, Place, Visit};
pub use profile::{
    discover_profiles, find_profile, get_default_profile, list_all_profiles, Browser, Profile,
};
