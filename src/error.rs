//! Error type definitions

use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Profile not found: {browser} (name/path: {query})")]
    ProfileNotFound { browser: String, query: String },

    #[error("Multiple matching {browser} profiles found, please specify explicitly: {matches:?}")]
    AmbiguousProfile {
        browser: String,
        matches: Vec<String>,
    },

    #[error("Default profile not found: {browser}")]
    NoDefaultProfile { browser: String },

    #[error("Invalid profile directory: {path} (missing places.sqlite)")]
    InvalidProfileDir { path: PathBuf },

    #[error("Database locked (browser may be running): {path}")]
    DatabaseLocked { path: PathBuf },

    #[error("Backup confirmation failed: user cancelled")]
    BackupNotConfirmed,

    #[error("Source and destination profile are the same: {path}")]
    SameProfile { path: PathBuf },

    #[error("Migration validation failed: {detail}")]
    ValidationFailed { detail: String },

    #[error("Schema version mismatch: expected {expected}, actual {actual}")]
    SchemaMismatch { expected: i32, actual: i32 },

    #[error("Config parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("UTF-8 conversion error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("Path strip error: {0}")]
    PathStrip(#[from] std::path::StripPrefixError),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Other error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn is_db_locked(&self) -> bool {
        matches!(self, Error::DatabaseLocked { .. })
            || matches!(self, Error::Sqlite(e) if e.to_string().contains("database is locked"))
    }
}
