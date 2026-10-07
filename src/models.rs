// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

//! Data model definitions

use chrono::{DateTime, Utc};
use rusqlite::{Row, ToSql};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str;

/// Helper to read TEXT columns with lossy UTF-8 conversion
/// Firefox's places.sqlite may contain invalid UTF-8 in text fields
/// Handles both TEXT (String) and BLOB (Vec<u8>) column types
fn get_text_lossy(row: &Row, idx: usize) -> rusqlite::Result<Option<String>> {
    // Use get_ref to access raw value and handle invalid UTF-8
    let val = row.get_ref(idx)?;
    match val {
        rusqlite::types::ValueRef::Null => Ok(None),
        rusqlite::types::ValueRef::Text(bytes) => {
            Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
        }
        rusqlite::types::ValueRef::Blob(bytes) => {
            Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
        }
        _ => {
            // For other types (Integer, Real), convert to string
            Ok(Some(val.as_str()?.to_string()))
        }
    }
}

fn get_text_lossy_required(row: &Row, idx: usize) -> rusqlite::Result<String> {
    let val = row.get_ref(idx)?;
    match val {
        rusqlite::types::ValueRef::Null => Ok(String::new()),
        rusqlite::types::ValueRef::Text(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        rusqlite::types::ValueRef::Blob(bytes) => Ok(String::from_utf8_lossy(bytes).into_owned()),
        _ => {
            // For other types (Integer, Real), convert to string
            Ok(val.as_str()?.to_string())
        }
    }
}

/// Firefox/LibreWolf stores timestamps as UTC microseconds (PRTime)
pub type Microseconds = i64;

/// Convert microseconds timestamp to `DateTime<Utc>`
pub fn microseconds_to_datetime(us: Microseconds) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(us).unwrap_or_else(Utc::now)
}

/// Convert `DateTime<Utc>` to microseconds
pub fn datetime_to_microseconds(dt: DateTime<Utc>) -> Microseconds {
    dt.timestamp_micros()
}

/// Visit type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisitType(pub i32);

impl VisitType {
    pub const LINK: Self = Self(1);
    pub const TYPED: Self = Self(2);
    pub const BOOKMARK: Self = Self(3);
    pub const EMBED: Self = Self(4);
    pub const REDIRECT_PERMANENT: Self = Self(5);
    pub const REDIRECT_TEMPORARY: Self = Self(6);
    pub const DOWNLOAD: Self = Self(7);
    pub const FRAME: Self = Self(8);

    pub fn from_i32(v: i32) -> Self {
        Self(v)
    }

    pub fn as_i32(&self) -> i32 {
        self.0
    }
}

impl fmt::Display for VisitType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.0 {
            1 => "link",
            2 => "typed",
            3 => "bookmark",
            4 => "embed",
            5 => "redirect_permanent",
            6 => "redirect_temporary",
            7 => "download",
            8 => "frame",
            _ => return write!(f, "other({})", self.0),
        };
        write!(f, "{}", s)
    }
}

/// moz_origins table
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    pub id: i64,
    pub prefix: String,
    pub host: String,
    pub frecency: i64,
    pub recalc_frecency: i64,
    pub alt_frecency: Option<i64>,
    pub recalc_alt_frecency: i64,
    pub block_until_ms: Option<i64>,
    pub block_pages_until_ms: Option<i64>,
}

impl Origin {
    pub fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            prefix: get_text_lossy_required(row, 1)?,
            host: get_text_lossy_required(row, 2)?,
            frecency: row.get(3)?,
            recalc_frecency: row.get(4)?,
            alt_frecency: row.get(5)?,
            recalc_alt_frecency: row.get(6)?,
            block_until_ms: row.get(7)?,
            block_pages_until_ms: row.get(8)?,
        })
    }

    pub fn to_insert_params(&self) -> Vec<Box<dyn ToSql>> {
        vec![
            Box::new(self.prefix.clone()),
            Box::new(self.host.clone()),
            Box::new(self.frecency),
            Box::new(self.recalc_frecency),
            Box::new(self.alt_frecency),
            Box::new(self.recalc_alt_frecency),
            Box::new(self.block_until_ms),
            Box::new(self.block_pages_until_ms),
        ]
    }

    /// Unique key: (host, prefix)
    pub fn unique_key(&self) -> String {
        format!("{} {}", self.prefix, self.host)
    }
}

/// moz_places table
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Place {
    pub id: i64,
    pub url: String,
    pub title: Option<String>,
    pub rev_host: String,
    pub visit_count: i64,
    pub hidden: bool,
    pub typed: i64,
    pub frecency: i64,
    pub last_visit_date: Option<Microseconds>,
    pub guid: String,
    pub foreign_count: i64,
    pub url_hash: i64,
    pub description: Option<String>,
    pub preview_image_url: Option<String>,
    pub site_name: Option<String>,
    pub origin_id: Option<i64>,
    pub recalc_frecency: i64,
    pub alt_frecency: Option<i64>,
    pub recalc_alt_frecency: i64,
}

impl Place {
    pub fn from_row(row: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            url: get_text_lossy_required(row, 1)?,
            title: get_text_lossy(row, 2)?,
            rev_host: get_text_lossy_required(row, 3)?,
            visit_count: row.get(4)?,
            hidden: row.get(5)?,
            typed: row.get(6)?,
            frecency: row.get(7)?,
            last_visit_date: row.get(8)?,
            guid: get_text_lossy_required(row, 9)?,
            foreign_count: row.get(10)?,
            url_hash: row.get(11)?,
            description: get_text_lossy(row, 12)?,
            preview_image_url: get_text_lossy(row, 13)?,
            site_name: get_text_lossy(row, 14)?,
            origin_id: row.get(15)?,
            recalc_frecency: row.get(16)?,
            alt_frecency: row.get(17)?,
            recalc_alt_frecency: row.get(18)?,
        })
    }

    pub fn to_insert_params(&self) -> Vec<Box<dyn ToSql>> {
        vec![
            Box::new(self.url.clone()),
            Box::new(self.title.clone()),
            Box::new(self.rev_host.clone()),
            Box::new(self.visit_count),
            Box::new(self.hidden),
            Box::new(self.typed),
            Box::new(self.frecency),
            Box::new(self.last_visit_date),
            Box::new(self.guid.clone()),
            Box::new(self.foreign_count),
            Box::new(self.url_hash),
            Box::new(self.description.clone()),
            Box::new(self.preview_image_url.clone()),
            Box::new(self.site_name.clone()),
            Box::new(self.origin_id),
            Box::new(self.recalc_frecency),
            Box::new(self.alt_frecency),
            Box::new(self.recalc_alt_frecency),
        ]
    }

    /// Dedup key: url_hash + url
    pub fn dedup_key(&self) -> String {
        format!("{}:{}", self.url_hash, self.url)
    }

    /// Merge another Place into self (for dedup merging)
    pub fn merge(&mut self, other: &Place) {
        self.visit_count = self.visit_count.saturating_add(other.visit_count);
        self.typed = self.typed.max(other.typed);
        self.hidden = self.hidden || other.hidden;
        self.foreign_count = self.foreign_count.saturating_add(other.foreign_count);

        // Merge last_visit_date taking max (latest)
        // Note: first visit time would need to be derived from visits table
        self.last_visit_date = match (self.last_visit_date, other.last_visit_date) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        // frecency recalculation left for later unified calculation
    }
}

/// moz_historyvisits table
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Visit {
    pub id: i64,
    pub from_visit: Option<i64>,
    pub place_id: i64,
    pub visit_date: Microseconds,
    pub visit_type: VisitType,
    pub session: i64,
    pub source: i32,
    pub triggering_place_id: Option<i64>,
}

impl Visit {
    pub fn from_row(row: &Row) -> rusqlite::Result<Self> {
        let visit_type_raw: i32 = row.get(4)?;
        Ok(Self {
            id: row.get(0)?,
            from_visit: row.get(1)?,
            place_id: row.get(2)?,
            visit_date: row.get(3)?,
            visit_type: VisitType::from_i32(visit_type_raw),
            session: row.get(5)?,
            source: row.get(6)?,
            triggering_place_id: row.get(7)?,
        })
    }

    pub fn to_insert_params(&self) -> Vec<Box<dyn ToSql>> {
        vec![
            Box::new(self.from_visit),
            Box::new(self.place_id),
            Box::new(self.visit_date),
            Box::new(self.visit_type.as_i32()),
            Box::new(self.session),
            Box::new(self.source),
            Box::new(self.triggering_place_id),
        ]
    }

    /// Dedup key: place_id + visit_date + visit_type
    ///
    /// This is the single source of truth for visit dedup keys — seeding the
    /// dedup set from the destination DB must call [`Visit::key_of`] with the
    /// same components, or cross-run visit dedup silently never matches.
    pub fn dedup_key(&self) -> String {
        Self::key_of(self.place_id, self.visit_date, self.visit_type.as_i32())
    }

    /// Build a visit dedup key from raw components (see [`Visit::dedup_key`]).
    pub fn key_of(place_id: i64, visit_date: i64, visit_type: i32) -> String {
        format!("{}:{}:{}", place_id, visit_date, visit_type)
    }
}

/// moz_meta table key-value pair
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub key: String,
    pub value: String,
}

/// Migration statistics
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MigrationStats {
    pub origins_read: usize,
    pub origins_inserted: usize,
    pub origins_merged: usize,
    pub places_read: usize,
    pub places_inserted: usize,
    pub places_merged: usize,
    pub places_skipped: usize,
    pub visits_read: usize,
    pub visits_inserted: usize,
    pub visits_skipped: usize,
    pub duration_ms: u64,
}

impl MigrationStats {
    pub fn print_summary(&self) {
        println!("\n=== Migration Statistics ===");
        println!(
            "Origins:  read={}, inserted={}, merged={}",
            self.origins_read, self.origins_inserted, self.origins_merged
        );
        println!(
            "Places:   read={}, inserted={}, merged={}, skipped={}",
            self.places_read, self.places_inserted, self.places_merged, self.places_skipped
        );
        println!(
            "Visits:   read={}, inserted={}, skipped={}",
            self.visits_read, self.visits_inserted, self.visits_skipped
        );
        println!("Duration: {} ms", self.duration_ms);
    }
}
