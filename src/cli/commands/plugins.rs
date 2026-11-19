//! Plugins command implementation.
//!
//! The command follows Rez's package-plugin contract. A package is a plugin
//! for another package only when its package metadata declares that relation
//! through the plugin_for field.

use clap::Args;
use rez_next_common::{RezCoreError, config::RezCoreConfig, error::RezCoreResult};
use rez_next_package::Package;
use rez_next_repository::{
    get_plugin_packages, package_repository::spec::resolve_filesystem_paths,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Arguments for the plugins command.
#[derive(Args, Clone, Debug)]
pub struct PluginsArgs {
    /// Package to list plugins for.
    #[arg(
        value_name = "PKG",
        add = clap_complete::ArgValueCompleter::new(
            crate::cli::commands::completions::package_request_candidates
        )
    )]
    pub package: Option<String>,

    /// Set package search path.
    #[arg(long = "paths", value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// Include package metadata in the output.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
}

/// Plugin package information used by the command renderer.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    pub name: String,
    pub package_name: String,
    pub package_version: Option<String>,
    pub plugin_type: String,
    pub description: Option<String>,
    pub module_path: Option<String>,
    pub plugin_for: Vec<String>,
}

/// Plugin discovery result.
#[derive(Debug, Clone)]
pub struct PluginDiscoveryResult {
    pub plugins: Vec<PluginInfo>,
    pub package_name: String,
    pub total_found: usize,
}

/// Execute the plugins command.
pub fn execute(args: PluginsArgs) -> RezCoreResult<()> {
    let package_name = args
        .package
        .as_deref()
        .ok_or_else(|| RezCoreError::RequirementParse("a package name is required".to_string()))?;
    let paths = package_paths(&args)?;
    let packages = get_plugin_packages(package_name, Some(paths));
    let discovery_result = PluginDiscoveryResult {
        total_found: packages.len(),
        plugins: packages.into_iter().map(plugin_info).collect(),
        package_name: package_name.to_string(),
    };

    display_plugins_result(&discovery_result, args.verbose);
    Ok(())
}

fn package_paths(args: &PluginsArgs) -> RezCoreResult<Vec<String>> {
    let entries: Vec<String> = if args.paths.is_empty() {
        let config = RezCoreConfig::load();
        let mut paths = config.packages_path;
        if !paths.contains(&config.local_packages_path) {
            paths.push(config.local_packages_path);
        }
        paths
    } else {
        args.paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    };

    Ok(resolve_filesystem_paths(entries)?
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect())
}

fn plugin_info(package: Package) -> PluginInfo {
    PluginInfo {
        name: package.name.clone(),
        package_name: package.name,
        package_version: package.version.map(|version| version.as_str().to_string()),
        plugin_type: "package".to_string(),
        description: package.description,
        module_path: package.filepath,
        plugin_for: package.plugin_for,
    }
}

fn display_plugins_result(result: &PluginDiscoveryResult, verbose: bool) {
    if result.plugins.is_empty() {
        eprintln!("package '{}' has no plugins.", result.package_name);
        return;
    }

    if !verbose {
        for plugin in &result.plugins {
            println!("{}", plugin.name);
        }
        return;
    }

    let mut plugins_by_type: BTreeMap<&str, Vec<&PluginInfo>> = BTreeMap::new();
    for plugin in &result.plugins {
        plugins_by_type
            .entry(plugin.plugin_type.as_str())
            .or_default()
            .push(plugin);
    }

    println!("Plugins found for package '{}':", result.package_name);
    for (plugin_type, plugins) in plugins_by_type {
        println!("{} plugins:", plugin_type.replace('_', " "));
        for plugin in plugins {
            println!("  {}", plugin.name);
            if let Some(description) = &plugin.description {
                println!("    Description: {description}");
            }
            if let Some(version) = &plugin.package_version {
                println!("    Version: {version}");
            }
            if !plugin.plugin_for.is_empty() {
                println!("    Plugin for: {}", plugin.plugin_for.join(", "));
            }
            if let Some(module_path) = &plugin.module_path {
                println!("    Definition: {module_path}");
            }
        }
    }
    println!("Total: {} plugins found", result.total_found);
}
