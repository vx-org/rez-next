//! Utility helpers for the bind command.
//!
//! - [`BindModule`]          — descriptor for a known bind module
//! - [`get_bind_modules`]    — enumerate all built-in bind modules
//! - [`which_executable`]    — locate an executable on PATH
//! - [`extract_version_from_output`] — pull a version string out of CLI output
//! - [`parse_version_from_string`]   — parse the first version-like token
//! - [`find_close_matches`]  — substring-based fuzzy name matching

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::utils::expand_home_path;
use rez_next_bind::{BuiltinBinder, builtin_binders};
use rez_next_common::error::RezCoreResult;

/// Bind module information
#[derive(Debug, Clone)]
pub struct BindModule {
    /// Module name
    pub name: String,
    /// Module file path, or a stable built-in descriptor marker.
    pub path: PathBuf,
    /// Optional human-readable description
    pub description: Option<String>,
    /// Platforms on which this module is supported
    pub platforms: Vec<String>,
}

/// Return all known built-in bind modules.
pub fn get_bind_modules() -> RezCoreResult<HashMap<String, BindModule>> {
    let mut modules = builtin_binders()
        .iter()
        .map(builtin_module)
        .chain([
            ("platform", "System platform package"),
            ("arch", "System architecture package"),
            ("os", "Operating system package"),
            ("rez", "Rez package manager"),
            ("setuptools", "Python setuptools"),
        ])
        .map(|(name, description)| {
            (
                name.to_string(),
                BindModule {
                    name: name.to_string(),
                    path: PathBuf::from("<built-in>"),
                    description: Some(description.to_string()),
                    platforms: all_platforms(),
                },
            )
        })
        .collect::<HashMap<_, _>>();

    for module in discover_bind_modules(&configured_bind_module_paths()) {
        modules.entry(module.name.clone()).or_insert(module);
    }

    Ok(modules)
}

fn builtin_module(binder: &BuiltinBinder) -> (&str, &str) {
    (binder.name, binder.description)
}

fn all_platforms() -> Vec<String> {
    vec![
        "windows".to_string(),
        "linux".to_string(),
        "darwin".to_string(),
    ]
}

fn configured_bind_module_paths() -> Vec<PathBuf> {
    let config = rez_next_common::config::RezCoreConfig::load();
    config
        .get_field("bind_module_path")
        .and_then(|value| value.as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(expand_home_path))
        .collect()
}

fn discover_bind_modules(paths: &[PathBuf]) -> Vec<BindModule> {
    let mut modules = Vec::new();

    for path in paths {
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };

        let mut files = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        files.sort();
        for file in files {
            if !is_bind_module_file(&file) {
                continue;
            }

            let Some(name) = file.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };

            modules.push(BindModule {
                name: name.to_string(),
                path: file,
                description: None,
                platforms: all_platforms(),
            });
        }
    }

    modules
}

fn is_bind_module_file(path: &Path) -> bool {
    path.is_file()
        && path.extension().and_then(|extension| extension.to_str()) == Some("py")
        && path.file_stem().and_then(|stem| stem.to_str()) != Some("__init__")
}

/// Find close matches for `name` among the known modules using substring containment.
pub fn find_close_matches<'a>(
    name: &str,
    modules: &'a HashMap<String, BindModule>,
) -> Vec<(String, &'a BindModule)> {
    let mut matches: Vec<(String, &BindModule)> = modules
        .iter()
        .filter(|(k, _)| k.contains(name) || name.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v))
        .collect();

    matches.sort_by(|a, b| a.0.cmp(&b.0));
    matches
}

/// Locate an executable on `PATH` using `which` (Unix) or `where` (Windows).
pub fn which_executable(cmd: &str) -> Option<PathBuf> {
    let which_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = Command::new(which_cmd).arg(cmd).output()
        && output.status.success()
    {
        let path_str = String::from_utf8_lossy(&output.stdout);
        let first_line = path_str.lines().next().unwrap_or("").trim();
        if !first_line.is_empty() {
            return Some(PathBuf::from(first_line));
        }
    }
    None
}

/// Extract a version string from CLI output that contains a known `prefix`.
///
/// Example: `extract_version_from_output("Python 3.9.7\n", "Python")` → `Some("3.9.7")`
pub fn extract_version_from_output(output: &str, prefix: &str) -> Option<String> {
    let lower_output = output.to_lowercase();
    let lower_prefix = prefix.to_lowercase();

    for (line_lower, line_orig) in lower_output.lines().zip(output.lines()) {
        if let Some(pos) = line_lower.find(&lower_prefix) {
            let rest = line_orig[pos + prefix.len()..].trim_start();
            if let Some(ver) = parse_version_from_string(rest) {
                return Some(ver);
            }
        }
    }
    None
}

/// Parse the first version-like token (`digits[.digits]*`) from `s`.
///
/// Returns `None` when no digit sequence is found.
pub fn parse_version_from_string(s: &str) -> Option<String> {
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();

    let start = chars.iter().position(|c| c.is_ascii_digit())?;

    let mut end = start;
    while end < chars.len()
        && (chars[end].is_ascii_digit()
            || chars[end] == '.'
            || chars[end] == '-'
            || chars[end] == '_')
    {
        end += 1;
    }

    let version_str: String = chars[start..end]
        .iter()
        .collect::<String>()
        .trim_end_matches(['.', '-', '_'])
        .to_string();

    if version_str.is_empty() {
        None
    } else {
        Some(version_str)
    }
}
