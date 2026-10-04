//! CLI entry point

use clap::Parser;
use fox2wolf::error::Result;
use fox2wolf::migrate::{migrate, MigrationContext};
use fox2wolf::profile::{discover_profiles, find_profile, get_default_profile, Browser, Profile};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "fox2wolf",
    version,
    about = "Firefox to LibreWolf history migration tool",
    long_about = "Migrate Firefox history (places.sqlite) to LibreWolf.\n\nSupports merge deduplication: same URL merges visit_count, preserves earliest/latest visit times.\n\n⚠️  Before migration, ensure Firefox and LibreWolf are completely closed, and manually backup LibreWolf's places.sqlite."
)]
struct Args {
    /// Firefox profile name or path (default: auto-detect default profile)
    #[arg(long, short = 'f', value_name = "NAME|PATH")]
    firefox_profile: Option<String>,

    /// LibreWolf profile name or path (default: auto-detect default profile)
    #[arg(long, short = 'l', value_name = "NAME|PATH")]
    librewolf_profile: Option<String>,

    /// Dry run only, no writes to destination
    #[arg(long)]
    dry_run: bool,

    /// Skip confirmation prompt, execute directly
    #[arg(long, short = 'y')]
    yes: bool,

    /// List all available Firefox/LibreWolf profiles and exit
    #[arg(long)]
    list_profiles: bool,

    /// Log level
    #[arg(long, default_value = "info", value_parser = ["trace", "debug", "info", "warn", "error"])]
    log_level: String,

    /// Disable progress bar
    #[arg(long)]
    no_progress: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    init_logging(&args.log_level)?;

    // List profiles
    if args.list_profiles {
        return list_profiles_cmd();
    }

    // Resolve source profile (Firefox)
    let src_profile = resolve_profile(Browser::Firefox, args.firefox_profile.as_deref())?;
    info!(
        "Source Profile: {} ({})",
        src_profile.name,
        src_profile.path.display()
    );

    // Resolve destination profile (LibreWolf)
    let dst_profile = resolve_profile(Browser::LibreWolf, args.librewolf_profile.as_deref())?;
    info!(
        "Destination Profile: {} ({})",
        dst_profile.name,
        dst_profile.path.display()
    );

    // Check if browsers are closed
    check_browsers_closed()?;

    // Create migration context
    let mut ctx = MigrationContext::new(src_profile, dst_profile, args.dry_run);
    ctx.skip_confirmation = args.yes;

    // Execute migration
    println!("\n🦊 Firefox → LibreWolf History Migration");
    println!("=========================================\n");

    let stats = migrate(&ctx)?;

    // Output results
    stats.print_summary();

    if args.dry_run {
        println!("\n[DRY RUN] Complete, destination not modified. Run without --dry-run for actual migration.");
    } else {
        println!("\n✅ Migration complete! Start LibreWolf to verify history.");
    }

    Ok(())
}

fn init_logging(level: &str) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap();

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .init();

    Ok(())
}

fn resolve_profile(browser: Browser, query: Option<&str>) -> Result<Profile> {
    match query {
        Some(q) => find_profile(browser, q),
        None => get_default_profile(browser),
    }
}

fn list_profiles_cmd() -> Result<()> {
    println!("Available Profiles:\n");

    for browser in [Browser::Firefox, Browser::LibreWolf] {
        println!("=== {} ===", browser.as_str());
        let profiles = discover_profiles(browser)?;
        if profiles.is_empty() {
            println!("  (none found)");
        } else {
            for p in profiles {
                let default_mark = if p.is_default { " ⭐ (default)" } else { "" };
                println!("  - {} [{}]", p.name, p.path.display());
                println!("    Path: {}{}", p.path.display(), default_mark);
            }
        }
        println!();
    }
    Ok(())
}

fn check_browsers_closed() -> Result<()> {
    // Can only check lock files, cannot force check processes
    // Actual lock check happens in profile.validate()
    warn!("Ensure Firefox and LibreWolf are completely closed!");
    warn!("Migration will fail due to database lock if browsers are running.");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_resolve_profile_default() {
        // Requires actual environment, skipped in unit tests
    }
}
