//! Package search functionality for Rez.
//!
//! This module provides package search capabilities, including:
//! - Searching for packages by name, version, or other criteria
//! - Finding reverse dependencies (packages that depend on a given package)
//! - Finding plugins for a given package
//!
//! This aligns with Rez's `package_search.py` interface.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rez_next_package::package::Package;

/// Result of a resource search.
///
/// This aligns with Rez's `ResourceSearchResult` class.
#[derive(Debug, Clone)]
pub struct ResourceSearchResult {
    /// The resource (package name as string, or Package object)
    pub resource: String,

    /// Type of resource: "family", "package", or "variant"
    pub resource_type: String,

    /// Validation error, if any
    pub validation_error: Option<String>,
}

impl ResourceSearchResult {
    /// Create a new ResourceSearchResult.
    pub fn new(resource: String, resource_type: String) -> Self {
        Self {
            resource,
            resource_type,
            validation_error: None,
        }
    }

    /// Set validation error.
    pub fn with_error(mut self, error: String) -> Self {
        self.validation_error = Some(error);
        self
    }
}

/// Search for plugins of a given package.
///
/// This aligns with Rez's `get_plugins()` function.
///
/// # Arguments
/// * `package_name` - Name of the package to find plugins for
/// * `paths` - Repository paths to search (uses default paths if None)
///
/// # Returns
/// List of plugin package names.
///
/// # Example
/// ```
/// use rez_next_repository::package_search::get_plugins;
///
/// // This would need actual repository paths to work
/// // let plugins = get_plugins("maya", None);
/// ```
pub fn get_plugins(package_name: &str, paths: Option<Vec<String>>) -> Vec<String> {
    get_plugin_packages(package_name, paths)
        .into_iter()
        .map(|package| package.name)
        .collect()
}

/// Load the latest package metadata for every package family that targets a
/// package through its `plugin_for` field.
///
/// A package family can contain versions with different plugin metadata. The
/// scan therefore considers every version and retains the highest matching
/// version for each family. Repository order is deterministic, while the
/// returned packages are sorted by package name.
pub fn get_plugin_packages(package_name: &str, paths: Option<Vec<String>>) -> Vec<Package> {
    let search_paths = paths.unwrap_or_else(get_default_package_paths);
    let mut packages_by_name: HashMap<String, Package> = HashMap::new();

    for path in search_paths {
        let path = Path::new(&path);
        if !path.is_dir() {
            continue;
        }

        let mut family_paths = sorted_directories(path);
        for family_path in family_paths.drain(..) {
            let mut matching_packages =
                sorted_package_versions(&family_path)
                    .into_iter()
                    .filter(|package| {
                        package
                            .plugin_for
                            .iter()
                            .any(|target| target == package_name)
                    });

            let Some(package) = matching_packages.next() else {
                continue;
            };

            let should_replace = packages_by_name
                .get(&package.name)
                .is_none_or(|current| compare_package_versions(&package, current).is_gt());
            if should_replace {
                packages_by_name.insert(package.name.clone(), package);
            }
        }
    }

    let mut packages: Vec<Package> = packages_by_name.into_values().collect();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    packages
}

/// Load a package from a version directory.
fn load_package_from_dir(dir: &Path) -> Option<Package> {
    ["package.py", "package.yaml", "package.yml", "package.json"]
        .iter()
        .map(|filename| dir.join(filename))
        .find(|path| path.is_file())
        .and_then(|path| Package::from_path(path).ok())
}

fn sorted_directories(path: &Path) -> Vec<std::path::PathBuf> {
    let mut directories: Vec<_> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|entry| entry.is_dir())
        .collect();
    directories.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
    directories
}

fn sorted_package_versions(family_path: &Path) -> Vec<Package> {
    let mut packages: Vec<_> = sorted_directories(family_path)
        .into_iter()
        .filter_map(|version_path| load_package_from_dir(&version_path))
        .collect();
    packages.sort_by(|left, right| compare_package_versions(right, left));
    packages
}

fn compare_package_versions(left: &Package, right: &Package) -> std::cmp::Ordering {
    match (&left.version, &right.version) {
        (Some(left), Some(right)) => left.cmp(right),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => left.filepath.as_deref().cmp(&right.filepath.as_deref()),
    }
}

/// Get the reverse dependency tree for a package.
///
/// This aligns with Rez's `get_reverse_dependency_tree()` function.
///
/// # Arguments
/// * `package_name` - The package to find reverse dependencies for
/// * `depth` - Maximum depth to search (None for unlimited)
/// * `paths` - Repository paths to search
/// * `build_requires` - Whether to include build_requires
/// * `private_build_requires` - Whether to include private_build_requires
///
/// # Returns
/// A tuple of (layers, graph) where:
/// - layers: `Vec<Vec<String>>` - Packages grouped by dependency depth
/// - graph: `HashMap<String, Vec<String>>` - Adjacency list of dependencies
///
/// # Example
/// ```
/// use rez_next_repository::package_search::get_reverse_dependency_tree;
///
/// // This would need actual repository paths to work
/// // let (layers, graph) = get_reverse_dependency_tree("python", None, None, false, false);
/// ```
pub fn get_reverse_dependency_tree(
    package_name: &str,
    depth: Option<usize>,
    paths: Option<Vec<String>>,
    build_requires: bool,
    _private_build_requires: bool,
) -> (Vec<Vec<String>>, HashMap<String, Vec<String>>) {
    let search_paths = paths.unwrap_or_else(get_default_package_paths);

    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut layers: Vec<Vec<String>> = Vec::new();
    let mut visited = HashSet::new();

    // Layer 0: the package itself
    layers.push(vec![package_name.to_string()]);
    visited.insert(package_name.to_string());

    // BFS to build reverse dependency tree
    let mut current_layer: Vec<String> = vec![package_name.to_string()];
    let mut current_depth = 0;

    while !current_layer.is_empty() && depth.is_none_or(|d| current_depth < d) {
        let mut next_layer = Vec::new();

        for pkg_name in &current_layer {
            // Find packages that depend on pkg_name
            let dependents = find_dependents(pkg_name, &search_paths, build_requires);

            for dep in dependents {
                if !visited.contains(&dep) {
                    visited.insert(dep.clone());
                    next_layer.push(dep.clone());

                    // Add to graph
                    graph.entry(pkg_name.clone()).or_default().push(dep);
                }
            }
        }

        current_layer = next_layer.clone();
        if !next_layer.is_empty() {
            layers.push(next_layer);
        }
        current_depth += 1;
    }

    (layers, graph)
}

/// Find packages that depend on a given package.
fn find_dependents(
    package_name: &str,
    paths: &[String],
    include_build_requires: bool,
) -> Vec<String> {
    let mut dependents = Vec::new();
    let mut visited = HashSet::new();

    for path in paths {
        let path = Path::new(path);
        if !path.exists() {
            continue;
        }

        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let family_path = entry.path();
                if !family_path.is_dir() {
                    continue;
                }

                let family_name = family_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                if visited.contains(&family_name) {
                    continue;
                }

                // Check if this package depends on package_name
                if package_depends_on(&family_path, package_name, include_build_requires) {
                    dependents.push(family_name.clone());
                    visited.insert(family_name);
                }
            }
        }
    }

    dependents.sort();
    dependents
}

/// Check if a package depends on a given package.
fn package_depends_on(
    family_path: &Path,
    target_package: &str,
    include_build_requires: bool,
) -> bool {
    // Check version directories
    if let Ok(entries) = std::fs::read_dir(family_path) {
        for entry in entries.flatten() {
            let version_path = entry.path();
            if !version_path.is_dir() {
                continue;
            }

            // Try to load package from this version
            if let Some(pkg) = load_package_from_dir(&version_path) {
                // Check requires (direct field on Package struct)
                for req in &pkg.requires {
                    if req.contains(target_package) {
                        return true;
                    }
                }

                // Check build_requires if requested (direct field on Package struct)
                if include_build_requires {
                    for req in &pkg.build_requires {
                        if req.contains(target_package) {
                            return true;
                        }
                    }
                }
            }

            // Only check the first version
            break;
        }
    }

    false
}

/// Get default package paths from environment or common locations.
fn get_default_package_paths() -> Vec<String> {
    // Check REZ_PACKAGES_PATH environment variable
    if let Ok(paths_str) = std::env::var("REZ_PACKAGES_PATH") {
        return paths_str
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }

    // Default paths
    vec!["./packages".to_string(), "~/.rez/packages".to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resource_search_result_creation() {
        let result = ResourceSearchResult::new("python".to_string(), "family".to_string());
        assert_eq!(result.resource, "python");
        assert_eq!(result.resource_type, "family");
        assert!(result.validation_error.is_none());
    }

    #[test]
    fn test_resource_search_result_with_error() {
        let result = ResourceSearchResult::new("python".to_string(), "family".to_string())
            .with_error("validation failed".to_string());
        assert_eq!(
            result.validation_error,
            Some("validation failed".to_string())
        );
    }

    #[test]
    fn test_get_plugins_empty_paths() {
        let plugins = get_plugins("nonexistent", Some(vec![]));
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_get_plugins_nonexistent_path() {
        let plugins = get_plugins("python", Some(vec!["C:\\nonexistent\\path".to_string()]));
        assert!(plugins.is_empty());
    }

    #[test]
    fn test_get_reverse_dependency_tree_empty_paths() {
        let (layers, graph) =
            get_reverse_dependency_tree("python", None, Some(vec![]), false, false);
        assert_eq!(layers.len(), 1); // Only the package itself
        assert_eq!(layers[0], vec!["python"]);
        assert!(graph.is_empty());
    }

    #[test]
    fn test_get_reverse_dependency_tree_nonexistent_package() {
        let (layers, graph) = get_reverse_dependency_tree(
            "nonexistent_package_xyz",
            None,
            Some(vec!["./nonexistent".to_string()]),
            false,
            false,
        );
        assert_eq!(layers.len(), 1); // Only the package itself
        assert!(graph.is_empty());
    }
}
