# fox2wolf

> Firefox → LibreWolf history migration tool

Migrate Firefox's `places.sqlite` history (visits, URLs, timestamps, frecency, etc.) to LibreWolf with **merge deduplication**, **single-transaction processing for large datasets**, and **progress display**.

**Language**: [English](README.md) | [简体中文](README.zh-CN.md)

## Features

- **Merge deduplication**: Same URL automatically merges `visit_count`, preserves earliest first visit and latest visit time
- **Large dataset optimization**: >500k records within a single transaction, disabled `synchronous`, WAL mode, progress bar
- **Auto Profile detection**: Reads `profiles.ini` to locate default Profile, supports manual override
- **Safety first**: Mandatory confirmation before migration, source DB read-only, destination DB transaction-protected, dry-run mode
- **UTC timestamp preservation**: Firefox PRTime (microseconds) migrated as-is, no timezone conversion
- **Cross-platform**: Works on Windows / Linux / macOS

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `rusqlite` | 0.31 | SQLite database access with bundled SQLite |
| `directories-next` | 2.0 | Cross-platform standard directory paths (AppData, etc.) |
| `clap` | 4.5 | CLI argument parsing |
| `uuid` | 1.8 | UUID v4 generation for new GUIDs |
| `chrono` | 0.4 | Date/time handling (UTC microseconds) |
| `crc32fast` | 1.3 | URL hash computation (Firefox-compatible) |
| `indicatif` | 0.17 | Progress bars |
| `tracing` + `tracing-subscriber` | 0.1 / 0.3 | Structured logging |
| `toml` | 0.8 | profiles.ini parsing |
| `walkdir` | 2.5 | Directory scanning for fallback Profile discovery |
| `serde` + `serde_json` | 1.0 | Serialization for stats/metadata |
| `thiserror` | 1.0 | Error handling |

## Installation

### Build from source

```bash
git clone https://github.com/lfstartwq/fox2wolf
cd fox2wolf
cargo build --release
# Binary at target/release/fox2wolf(.exe)
```

## Usage

### Basic usage (auto-detect default Profile)

```bash
fox2wolf
```

### Specify Profile

```bash
# Specify Firefox and LibreWolf profile names
fox2wolf --firefox-profile default-release --librewolf-profile default-default

# Or specify full paths
fox2wolf --firefox-profile "C:\Users\Name\AppData\Roaming\Mozilla\Firefox\Profiles\xyz.default-release" \
         --librewolf-profile "C:\Users\Name\AppData\Roaming\librewolf\Profiles\abc.default-default"
```

### Dry run (preview)

```bash
fox2wolf --dry-run
```

### Skip confirmation (automation)

```bash
fox2wolf --yes
```

### List all available Profiles

```bash
fox2wolf --list-profiles
```

### All options

```
Usage: fox2wolf [OPTIONS]

Options:
  -f, --firefox-profile <NAME|PATH>      Firefox profile name or path
  -l, --librewolf-profile <NAME|PATH>    LibreWolf profile name or path
      --dry-run                          Dry run only, no writes to destination
  -y, --yes                              Skip confirmation prompt
      --list-profiles                    List all profiles and exit
      --log-level <LEVEL>                Log level [trace, debug, info, warn, error] (default: info)
      --no-progress                      Disable progress bar
  -h, --help                             Show help
  -V, --version                          Show version
```

## Pre-migration checklist

**Important**:

1. **Completely close Firefox and LibreWolf** (check Task Manager, ensure no leftover processes)
2. **Manually backup LibreWolf's `places.sqlite`**:
   - Windows: `%APPDATA%\librewolf\Profiles\<profile>\places.sqlite`
   - Linux: `~/.librewolf/<profile>/places.sqlite`
   - macOS: `~/Library/Application Support/librewolf/Profiles/<profile>/places.sqlite`
3. Backup suggestion: rename to `places.sqlite.backup.$(date +%Y%m%d_%H%M%S)`

## What gets migrated

| Table | Description | Handling |
|-------|-------------|----------|
| `moz_origins` | Domain origins | Dedupe merge (host+prefix unique) |
| `moz_places` | URL records | Merge dedupe (url_hash+url), new GUID, recalc frecency |
| `moz_historyvisits` | Visit history | Dedupe (place_id+visit_date+visit_type), rewrite place_id mapping |

**Not migrated**: bookmarks, search keywords, input history, annotations, page metadata (export/import JSON manually if needed).

## Implementation details

### ID mapping strategy

- `moz_origins.id`: Dedupe by `(host, prefix)`, build `old_id → new_id` map
- `moz_places.id`: Dedupe by `(url_hash, url)`, build map, generate new UUID v4 as GUID
- `moz_historyvisits.id`: Auto-increment reassigned, `place_id` rewritten via the map recorded by `DedupContext::upsert_place`, `from_visit` copied verbatim from the source row

### Merge rules

When same URL exists:
- `visit_count` summed
- `hidden` logical OR
- `typed` max
- `foreign_count` summed
- `last_visit_date` max (latest visit)
- `frecency` recalculated after migration completes (see note below)

### UTF-8 handling

Firefox's `places.sqlite` may contain invalid UTF-8 sequences in TEXT columns (e.g., `description`). The tool uses `row.get_ref()` with lossy UTF-8 conversion to handle this gracefully.

## Develop

**Documentation**: [docs/architecture.md](docs/architecture.md) | [docs/development.md](docs/development.md)

### Code Structure

```
fox2wolf/
├── src/
│   ├── main.rs          # CLI entry point
│   ├── lib.rs           # Public API re-exports
│   ├── error.rs         # Error types
│   ├── models.rs        # Data models (Origin, Place, Visit, etc.)
│   ├── profile.rs       # Profile discovery & validation
│   ├── db.rs            # SQLite connections, PRAGMAs, transactions
│   ├── dedup.rs         # Merge/dedupe algorithms, ID remapping
│   └── migrate.rs       # Migration orchestration
├── tests/
│   └── integration_test.rs  # End-to-end tests
├── docs/
│   ├── architecture.md              # Architecture notes
│   ├── commit-message-guidelines.md # Commit message guidelines
│   └── development.md               # Development guide
├── Cargo.toml
├── Cargo.lock
├── README.md
└── README.zh-CN.md
```

## LICENSE

GPL-3.0-only