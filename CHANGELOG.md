# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Batched transaction support for large datasets (>100k rows) with `MAX_TX_ROWS` constant
- Profile discovery fallback validation using `prefs.js` existence check
- CI/CD pipeline with cross-platform testing (Windows, Linux, macOS)
- Chinese README (`README.zh-CN.md`)

### Changed
- Removed unused `scopeguard` dependency
- Simplified migration to single transaction (batched commits deferred to future release)
- Strengthened profile discovery fallback with `prefs.js` validation

### Documentation
- Added `docs/architecture.md` with data flow and ID remapping details
- Added `docs/development.md` with build/test commands and extension recipes
- Added `AGENTS.md` for AI agent onboarding
- Documented frecency calculation deviation from Firefox algorithm

## [0.1.0] - 2026-10-04

### Added
- Initial release of fox2wolf
- Firefox to LibreWolf history migration (`moz_origins`, `moz_places`, `moz_historyvisits`)
- Merge deduplication: same URL merges `visit_count`, preserves earliest/latest visit times
- Auto profile detection via `profiles.ini` with manual override support
- Dry-run mode (`--dry-run`) for safe preview
- Cross-platform support (Windows, Linux, macOS)
- Progress bar with `indicatif`
- Structured logging with `tracing`
- Dry-run and skip-confirmation (`-y`) flags
- Profile listing (`--list-profiles`)
- Integration tests with temp SQLite databases