//! Profile discovery and parsing

use crate::error::{Error, Result};
use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[cfg(windows)]
fn windows_appdata_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

/// Supported browser types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Browser {
    Firefox,
    LibreWolf,
}

impl Browser {
    pub fn as_str(&self) -> &'static str {
        match self {
            Browser::Firefox => "Firefox",
            Browser::LibreWolf => "LibreWolf",
        }
    }

    pub fn project_dirs_qualifier(&self) -> (&'static str, &'static str, &'static str) {
        match self {
            Browser::Firefox => ("Mozilla", "Firefox", ""),
            Browser::LibreWolf => ("librewolf", "librewolf", ""),
        }
    }

    pub fn profiles_dir(&self) -> Result<PathBuf> {
        let proj = ProjectDirs::from("org", "mozilla", self.as_str())
            .or_else(|| ProjectDirs::from("", "", self.project_dirs_qualifier().1))
            .ok_or_else(|| Error::Other("Unable to determine config directory".into()))?;
        Ok(proj.data_dir().join("Profiles"))
    }
}

/// Profile info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub path: PathBuf,
    pub is_default: bool,
    pub is_relative: bool,
    pub browser: Browser,
}

impl Profile {
    pub fn places_sqlite(&self) -> PathBuf {
        self.path.join("places.sqlite")
    }

    pub fn validate(&self) -> Result<()> {
        let places = self.places_sqlite();
        if !places.exists() {
            return Err(Error::InvalidProfileDir {
                path: self.path.clone(),
            });
        }
        Ok(())
    }
}

/// profiles.ini [ProfileX] section
#[derive(Debug, Clone, Deserialize)]
struct ProfileIniEntry {
    name: Option<String>,
    #[serde(rename = "IsRelative", deserialize_with = "deserialize_bool_or_int")]
    is_relative: Option<bool>,
    path: Option<String>,
    #[serde(rename = "Default")]
    default: Option<DefaultValue>,
    #[serde(rename = "Locked")]
    #[allow(dead_code)]
    locked: Option<String>,
}

fn deserialize_bool_or_int<'de, D>(deserializer: D) -> std::result::Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrInt {
        Bool(bool),
        Integer(i64),
        String(String),
    }

    let value = Option::<BoolOrInt>::deserialize(deserializer)?;
    Ok(value.map(|v| match v {
        BoolOrInt::Bool(b) => b,
        BoolOrInt::Integer(i) => i != 0,
        BoolOrInt::String(s) => s.parse::<i64>().map(|i| i != 0).unwrap_or(false),
    }))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum DefaultValue {
    String(String),
    Integer(i64),
    Bool(bool),
}

impl DefaultValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DefaultValue::String(s) => write!(f, "{}", s),
            DefaultValue::Integer(i) => write!(f, "{}", i),
            DefaultValue::Bool(b) => write!(f, "{}", b),
        }
    }
}

impl std::fmt::Display for DefaultValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.fmt(f)
    }
}

/// Read profiles.ini, returns entries and the profiles root directory
fn read_profiles_ini(browser: Browser) -> Result<(Vec<ProfileIniEntry>, PathBuf)> {
    let proj = ProjectDirs::from("org", "mozilla", browser.as_str())
        .or_else(|| ProjectDirs::from("", "", browser.project_dirs_qualifier().1))
        .ok_or_else(|| Error::Other("Unable to determine config directory".into()))?;

    let ini_path = proj.config_dir().join("profiles.ini");
    if ini_path.exists() {
        let entries = parse_ini_file(&ini_path)?;
        // Use the parent of profiles.ini as the profiles root
        let profiles_root = ini_path.parent().unwrap().join("Profiles");
        return Ok((entries, profiles_root));
    }

    // Try fallback: data_dir/profiles.ini (some distributions)
    let alt = proj.data_dir().join("profiles.ini");
    if alt.exists() {
        let entries = parse_ini_file(&alt)?;
        let profiles_root = alt.parent().unwrap().join("Profiles");
        return Ok((entries, profiles_root));
    }

    // Windows-specific fallback: check standard %APPDATA% locations
    #[cfg(windows)]
    {
        if let Some(appdata) = windows_appdata_dir() {
            let browser_dir = match browser {
                Browser::Firefox => appdata.join("Mozilla").join("Firefox"),
                Browser::LibreWolf => appdata.join("librewolf"),
            };
            let browser_ini = browser_dir.join("profiles.ini");
            if browser_ini.exists() {
                let entries = parse_ini_file(&browser_ini)?;
                let profiles_root = browser_dir.join("Profiles");
                return Ok((entries, profiles_root));
            }
        }
    }

    Err(Error::ProfileNotFound {
        browser: browser.as_str().into(),
        query: "profiles.ini not found".into(),
    })
}

fn parse_ini_file(path: &Path) -> Result<Vec<ProfileIniEntry>> {
    let content = std::fs::read_to_string(path)?;

    // Parse line by line to extract only Profile sections
    let mut profiles = Vec::new();
    let mut current_section = String::new();
    let mut current_fields: HashMap<String, String> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            // Save previous section if it was a Profile
            if current_section.starts_with("Profile") && !current_fields.is_empty() {
                let entry = parse_profile_entry(&current_fields)?;
                profiles.push(entry);
            }
            current_section = line[1..line.len() - 1].to_string();
            current_fields.clear();
        } else if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            let value = line[eq_pos + 1..]
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            current_fields.insert(key.to_string(), value.to_string());
        }
    }

    // Don't forget the last section
    if current_section.starts_with("Profile") && !current_fields.is_empty() {
        let entry = parse_profile_entry(&current_fields)?;
        profiles.push(entry);
    }

    Ok(profiles)
}

fn parse_profile_entry(fields: &HashMap<String, String>) -> Result<ProfileIniEntry> {
    // Convert HashMap to toml Value then deserialize
    let value = toml::Value::Table(
        fields
            .iter()
            .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
            .collect(),
    );
    value.try_into().map_err(Error::ConfigParse)
}

/// Parse all profiles from a list of ProfileIniEntry
fn parse_profiles(
    entries: Vec<ProfileIniEntry>,
    browser: Browser,
    profiles_root: &Path,
) -> Vec<Profile> {
    let mut profiles = Vec::new();

    for entry in entries {
        let name = entry.name.unwrap_or_else(|| "Unknown".to_string());
        let is_relative = entry.is_relative.unwrap_or(true);
        let path_str = match entry.path {
            Some(p) => p,
            None => continue,
        };

        let path = if is_relative {
            profiles_root.join(path_str)
        } else {
            PathBuf::from(path_str)
        };

        let is_default = entry.default.as_ref().map(|v| v.to_string()) == Some("1".to_string());

        profiles.push(Profile {
            name,
            path,
            is_default,
            is_relative,
            browser,
        });
    }

    profiles
}

/// Discover all profiles
pub fn discover_profiles(browser: Browser) -> Result<Vec<Profile>> {
    let (entries, profiles_root) = read_profiles_ini(browser)?;
    let mut profiles = parse_profiles(entries, browser, &profiles_root);

    // Fallback: scan all subdirectories in Profiles directory
    if profiles.is_empty() && profiles_root.exists() {
        for entry in WalkDir::new(&profiles_root)
            .max_depth(1)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_dir() {
                let path = entry.path().to_path_buf();
                // Verify it's a real Firefox/LibreWolf profile by checking for prefs.js
                if path.join("places.sqlite").exists() && path.join("prefs.js").exists() {
                    profiles.push(Profile {
                        name: entry.file_name().to_string_lossy().into_owned(),
                        path,
                        is_default: false,
                        is_relative: true,
                        browser,
                    });
                }
            }
        }
    }

    Ok(profiles)
}

/// Get default profile
pub fn get_default_profile(browser: Browser) -> Result<Profile> {
    let profiles = discover_profiles(browser)?;
    profiles
        .into_iter()
        .find(|p| p.is_default)
        .ok_or_else(|| Error::NoDefaultProfile {
            browser: browser.as_str().into(),
        })
}

/// Find profile by name or path
pub fn find_profile(browser: Browser, query: &str) -> Result<Profile> {
    let profiles = discover_profiles(browser)?;

    // 1. Exact name match
    let by_name: Vec<_> = profiles.iter().filter(|p| p.name == query).collect();
    if by_name.len() == 1 {
        return Ok(by_name[0].clone());
    }

    // 2. Path match
    let query_path = PathBuf::from(query);
    let by_path: Vec<_> = profiles
        .iter()
        .filter(|p| p.path == query_path || p.path.ends_with(&query_path))
        .collect();
    if by_path.len() == 1 {
        return Ok(by_path[0].clone());
    }

    // 3. Partial name match
    let by_partial: Vec<_> = profiles.iter().filter(|p| p.name.contains(query)).collect();
    if by_partial.len() == 1 {
        return Ok(by_partial[0].clone());
    }

    // Error handling
    let all_names: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();
    if by_name.len() > 1 || by_path.len() > 1 || by_partial.len() > 1 {
        let matches = by_name
            .into_iter()
            .chain(by_path)
            .chain(by_partial)
            .map(|p| p.name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        return Err(Error::AmbiguousProfile {
            browser: browser.as_str().into(),
            matches,
        });
    }

    if profiles.is_empty() {
        Err(Error::ProfileNotFound {
            browser: browser.as_str().into(),
            query: query.into(),
        })
    } else {
        Err(Error::ProfileNotFound {
            browser: browser.as_str().into(),
            query: format!("{} (available: {:?})", query, all_names),
        })
    }
}

/// List all profiles (for --list-profiles)
pub fn list_all_profiles() -> Result<Vec<Profile>> {
    let mut all = Vec::new();
    all.extend(discover_profiles(Browser::Firefox)?);
    all.extend(discover_profiles(Browser::LibreWolf)?);
    Ok(all)
}
