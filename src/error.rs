// Copyright (C) 2026 lfstartwq
// SPDX-License-Identifier: GPL-3.0-only

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

    #[error("Backup confirmation failed: user cancelled")]
    BackupNotConfirmed,

    #[error("Source and destination profile are the same: {path}")]
    SameProfile { path: PathBuf },

    #[error("Migration validation failed: {detail}")]
    ValidationFailed { detail: String },

    #[error("Config parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Other error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
