//! Package management functions exposed to Python.
//!
//! Covers: create_package, copy_package, move_package, remove_package.
//!
//! ## Architecture (SOLID / Clean Architecture):
//! These bindings are thin wrappers that delegate to the domain crate
//! (`rez_next_package`). Domain logic lives in `package_copy.rs`,
//! `package_move.rs`, and `package_remove.rs` — the Python bindings
//! only handle type conversion and error mapping.
//!
//! ## Lessons from Rez Issues (avoided pitfalls):
//! - **#1438 (UNC paths)**: Path normalization delegated to domain layer
//!   via `dunce::canonicalize`.
//! - **#1302 (case sensitivity)**: Platform-aware path comparisons in domain.
//! - **#1374 (filtered packages)**: Explicit target-only removal with dry-run.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::path::PathBuf;

use crate::package_bindings::PyPackage;
use crate::runtime::get_runtime;

use super::expand_home;

/// Create a Package object from a dictionary.
/// Equivalent to `rez.packages.create_package(data)`.
///
/// The dict must contain at least a "name" key.
/// Optional keys: version, description, authors, requires,
/// build_requires, variants, tools, commands, uuid, timestamp, etc.
#[pyfunction]
pub fn create_package(py: Python<'_>, data: Bound<'_, PyDict>) -> PyResult<PyPackage> {
    PyPackage::from_dict(py, data)
}

/// Resolve search paths from explicit paths or config.
fn resolve_search_paths(src_paths: &Option<Vec<String>>) -> Vec<PathBuf> {
    use rez_next_common::config::RezCoreConfig;

    match src_paths {
        Some(paths) if !paths.is_empty() => paths
            .iter()
            .map(|p| PathBuf::from(expand_home(p)))
            .collect(),
        _ => {
            let config = RezCoreConfig::load();
            config
                .packages_path
                .iter()
                .map(|p| PathBuf::from(expand_home(p)))
                .collect()
        }
    }
}

/// Find the latest version of a package from the repository.
fn find_latest_version(pkg_name: &str, src_paths: &Option<Vec<String>>) -> PyResult<String> {
    use super::make_repo_manager;

    let repo_manager = make_repo_manager(src_paths.clone());
    let rt = get_runtime();
    let packages = rt
        .block_on(repo_manager.find_packages(pkg_name))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    if packages.is_empty() {
        return Err(pyo3::exceptions::PyLookupError::new_err(format!(
            "Package '{}' not found in any search path",
            pkg_name
        )));
    }

    // Sort by version descending to find latest
    let mut sorted = packages;
    sorted.sort_by(|a, b| {
        b.version
            .as_ref()
            .and_then(|bv| a.version.as_ref().map(|av| bv.cmp(av)))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    sorted
        .first()
        .and_then(|p| p.version.as_ref())
        .map(|v| v.to_string())
        .ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!(
                "Package '{}' found but has no version",
                pkg_name
            ))
        })
}

fn find_source_package(
    pkg_name: &str,
    version: &str,
    search_paths: &[PathBuf],
) -> PyResult<(rez_next_package::Package, PathBuf)> {
    let manager = super::make_repo_manager(Some(
        search_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    ));
    let packages = get_runtime()
        .block_on(manager.find_packages(pkg_name))
        .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
    let package = packages
        .into_iter()
        .find(|candidate| {
            candidate
                .version
                .as_ref()
                .is_some_and(|candidate_version| candidate_version.as_str() == version)
        })
        .ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!(
                "Package '{}-{}' not found in any search path",
                pkg_name, version
            ))
        })?;
    let package = (*package).clone();

    if let Some(filepath) = &package.filepath {
        let source_path = PathBuf::from(filepath)
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| {
                pyo3::exceptions::PyRuntimeError::new_err(format!(
                    "Package '{}' has an invalid descriptor path",
                    pkg_name
                ))
            })?;
        if source_path.is_dir() {
            return Ok((package, source_path));
        }
    }

    for base in search_paths {
        let candidate = base.join(pkg_name).join(version);
        if candidate.is_dir() {
            return Ok((package, candidate));
        }
        let alternate = base.join(format!("{pkg_name}-{version}"));
        if alternate.is_dir() {
            return Ok((package, alternate));
        }
    }

    Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
        "Package '{}-{}' was indexed but its source directory was not found",
        pkg_name, version
    )))
}

fn select_variant_indices(
    package: &rez_next_package::Package,
    requested: Option<Vec<i32>>,
) -> PyResult<Vec<usize>> {
    let requested = requested.unwrap_or_else(|| {
        if package.variants.is_empty() {
            vec![0]
        } else {
            (0..package.variants.len())
                .map(|index| index as i32)
                .collect()
        }
    });
    let mut selected = Vec::new();
    for index in requested {
        let resolved = if package.variants.is_empty() && index == -1 {
            0
        } else if index < 0 {
            package.variants.len() as i32 + index
        } else {
            index
        };
        if resolved < 0
            || (package.variants.is_empty() && resolved != 0)
            || (!package.variants.is_empty() && resolved as usize >= package.variants.len())
        {
            return Err(pyo3::exceptions::PyIndexError::new_err(format!(
                "Package '{}' has no variant {}",
                package.name, index
            )));
        }
        let resolved = resolved as usize;
        if !selected.contains(&resolved) {
            selected.push(resolved);
        }
    }
    Ok(selected)
}

fn parse_overrides(
    overrides_json: Option<&str>,
) -> PyResult<Option<std::collections::HashMap<String, serde_json::Value>>> {
    overrides_json
        .map(|value| {
            serde_json::from_str(value).map_err(|error| {
                pyo3::exceptions::PyValueError::new_err(format!(
                    "Invalid package overrides: {error}"
                ))
            })
        })
        .transpose()
}

/// Copy a package through the filesystem repository installation contract.
#[pyfunction]
#[pyo3(signature = (pkg_name, dest_path, version=None, src_paths=None, force=false, variants=None, shallow=false, dest_name=None, dest_version=None, follow_symlinks=false, dry_run=false, keep_timestamp=false, skip_payload=false, overrides_json=None))]
pub fn copy_package_with_options(
    pkg_name: &str,
    dest_path: &str,
    version: Option<&str>,
    src_paths: Option<Vec<String>>,
    force: bool,
    variants: Option<Vec<i32>>,
    shallow: bool,
    dest_name: Option<&str>,
    dest_version: Option<&str>,
    follow_symlinks: bool,
    dry_run: bool,
    keep_timestamp: bool,
    skip_payload: bool,
    overrides_json: Option<&str>,
) -> PyResult<String> {
    let search_paths = resolve_search_paths(&src_paths);
    let source_version = match version {
        Some(version) => version.to_string(),
        None => find_latest_version(pkg_name, &src_paths)?,
    };
    let (package, source_path) = find_source_package(pkg_name, &source_version, &search_paths)?;
    let mut overrides = parse_overrides(overrides_json)?.unwrap_or_default();
    let override_name = overrides
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let override_version = overrides
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let destination_name = dest_name.or(override_name.as_deref()).unwrap_or(pkg_name);
    let destination_version = dest_version
        .or(override_version.as_deref())
        .unwrap_or(&source_version);
    if let Some(name) = dest_name {
        overrides.insert(
            "name".to_string(),
            serde_json::Value::String(name.to_string()),
        );
    }
    if let Some(version) = dest_version {
        overrides.insert(
            "version".to_string(),
            serde_json::Value::String(version.to_string()),
        );
    }

    let destination = PathBuf::from(super::expand_home(dest_path));
    let mut destination_descriptor = package.clone();
    destination_descriptor.name = destination_name.to_string();
    destination_descriptor.version = Some(
        rez_next_version::Version::parse(destination_version).map_err(|error| {
            pyo3::exceptions::PyValueError::new_err(format!(
                "Invalid destination package version '{destination_version}': {error}"
            ))
        })?,
    );
    let destination_package = destination.join(
        rez_next_package::PackageInstallLayout::package_base_relative_path(&destination_descriptor)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?,
    );
    let same_canonical_path = std::fs::canonicalize(&source_path)
        .ok()
        .zip(std::fs::canonicalize(&destination_package).ok())
        .is_some_and(|(source, destination)| source == destination);
    if source_path == destination_package || same_canonical_path {
        return Err(pyo3::exceptions::PyValueError::new_err(
            "Cannot copy a package over itself",
        ));
    }

    let selected_variants = select_variant_indices(&package, variants)?;
    if !dry_run {
        std::fs::create_dir_all(&destination).map_err(|error| {
            pyo3::exceptions::PyOSError::new_err(format!(
                "Failed to create destination repository: {error}"
            ))
        })?;
    }
    rez_next_repository::package_repository::filesystem::install_package_variants(
        &rez_next_repository::package_repository::filesystem::InstallPackageVariantsRequest {
            source_package: package,
            source_package_dir: source_path,
            destination_repository: destination.clone(),
            destination_name: destination_name.to_string(),
            destination_version: destination_version.to_string(),
            variant_indices: selected_variants,
            dry_run,
            overwrite: force,
            overrides: (!overrides.is_empty()).then_some(overrides),
            options: rez_next_repository::package_repository::InstallVariantOptions {
                keep_timestamp,
                shallow,
                follow_symlinks,
                skip_payload,
            },
        },
    )
    .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;

    Ok(destination_package.to_string_lossy().into_owned())
}

/// Move a package through the filesystem repository installation contract.
#[pyfunction]
#[pyo3(signature = (pkg_name, dest_path, version=None, src_paths=None, force=false, keep_source=false, dry_run=false, keep_timestamp=false))]
pub fn move_package_with_options(
    pkg_name: &str,
    dest_path: &str,
    version: Option<&str>,
    src_paths: Option<Vec<String>>,
    force: bool,
    keep_source: bool,
    dry_run: bool,
    keep_timestamp: bool,
) -> PyResult<String> {
    let source_version = match version {
        Some(version) => version.to_string(),
        None => find_latest_version(pkg_name, &src_paths)?,
    };
    let result = copy_package_with_options(
        pkg_name,
        dest_path,
        Some(&source_version),
        src_paths.clone(),
        force,
        None,
        false,
        None,
        None,
        false,
        dry_run,
        keep_timestamp,
        false,
        None,
    )?;
    if !keep_source && !dry_run {
        let config = rez_next_package::package_remove::PackageRemoveConfig {
            packages_path: resolve_search_paths(&src_paths),
            force: true,
            prune_empty_families: true,
        };
        rez_next_package::package_remove::remove_package_version(
            pkg_name,
            &source_version,
            &config,
        )
        .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
    }
    Ok(result)
}

/// Copy a package to another location.
/// Equivalent to `rez cp <pkg> <dest>` / `rez.copy_package(pkg, dest_repo_path)`
///
/// Delegates to `rez_next_package::package_copy::copy_package()` domain function.
#[pyfunction]
#[pyo3(signature = (pkg_name, dest_path, version=None, src_paths=None, force=false))]
pub fn copy_package(
    pkg_name: &str,
    dest_path: &str,
    version: Option<&str>,
    src_paths: Option<Vec<String>>,
    force: bool,
) -> PyResult<String> {
    use rez_next_package::package_copy::{PackageCopyConfig, copy_package as domain_copy};

    let search_paths = resolve_search_paths(&src_paths);

    // Determine version: use provided or find latest
    let ver_str: String = match version {
        Some(v) => v.to_string(),
        None => find_latest_version(pkg_name, &src_paths)?,
    };

    let config = PackageCopyConfig {
        packages_path: search_paths,
        force,
        normalize_paths: true,
    };

    let result = domain_copy(
        pkg_name,
        &ver_str,
        PathBuf::from(dest_path).as_path(),
        &config,
    )
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    Ok(result.destination.to_string_lossy().to_string())
}

/// Move a package to another location.
/// Equivalent to `rez mv <pkg> <dest>`
///
/// Delegates to `rez_next_package::package_move::move_package()` domain function
/// (which itself delegates to copy + remove — SRP).
#[pyfunction]
#[pyo3(signature = (pkg_name, dest_path, version=None, src_paths=None, force=false, keep_source=false))]
pub fn move_package(
    pkg_name: &str,
    dest_path: &str,
    version: Option<&str>,
    src_paths: Option<Vec<String>>,
    force: bool,
    keep_source: bool,
) -> PyResult<String> {
    use rez_next_package::package_move::{PackageMoveConfig, move_package as domain_move};

    let search_paths = resolve_search_paths(&src_paths);

    // Determine version: use provided or find latest
    let ver_str: String = match version {
        Some(v) => v.to_string(),
        None => find_latest_version(pkg_name, &src_paths)?,
    };

    let config = PackageMoveConfig {
        packages_path: search_paths,
        force,
        keep_source,
        normalize_paths: true,
    };

    let result = domain_move(
        pkg_name,
        &ver_str,
        PathBuf::from(dest_path).as_path(),
        &config,
    )
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    Ok(result.destination.to_string_lossy().to_string())
}

/// Remove a package version from repositories.
/// Equivalent to `rez rm <pkg>`
///
/// Delegates to `rez_next_package::package_remove` domain functions.
#[pyfunction]
#[pyo3(signature = (pkg_name, version=None, paths=None))]
pub fn remove_package(
    pkg_name: &str,
    version: Option<&str>,
    paths: Option<Vec<String>>,
) -> PyResult<usize> {
    use rez_next_package::package_remove::{
        PackageRemoveConfig, remove_package_family, remove_package_version,
    };

    let search_paths = resolve_search_paths(&paths);

    let config = PackageRemoveConfig {
        packages_path: search_paths,
        force: true,
        prune_empty_families: true,
    };

    if let Some(ver) = version {
        let result = remove_package_version(pkg_name, ver, &config)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(result.versions_removed)
    } else {
        let result = remove_package_family(pkg_name, &config)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        Ok(result.versions_removed)
    }
}
