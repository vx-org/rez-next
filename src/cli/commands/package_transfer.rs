//! Shared repository-backed implementation for `rez cp` and `rez mv`.

use crate::cli::utils::expand_home_path;
use rez_next_common::{RezCoreError, error::RezCoreResult};
use rez_next_package::{Package, PackageInstallLayout};
use rez_next_repository::package_repository::bridge::AsyncRepositoryAdapter;
use rez_next_repository::package_repository::{FilesystemPackageRepository, InstallVariantOptions};
use rez_next_repository::simple_repository::RepositoryManager;
use rez_next_version::Version;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub(crate) struct PackageTransferOptions {
    pub source_package: Option<String>,
    pub variant_uri: Option<String>,
    pub destination_path: Option<PathBuf>,
    pub source_paths: Vec<PathBuf>,
    pub variant_indices: Option<Vec<i32>>,
    pub force: bool,
    pub overwrite: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub keep_timestamp: bool,
    pub shallow: bool,
    pub follow_symlinks: bool,
    pub allow_empty: bool,
    pub rename: Option<String>,
    pub reversion: Option<String>,
    pub keep_source: bool,
}

#[derive(Debug)]
pub(crate) struct PackageTransferResult {
    pub destination_path: PathBuf,
    pub variants_processed: usize,
}

pub(crate) async fn transfer_package(
    options: &PackageTransferOptions,
) -> RezCoreResult<PackageTransferResult> {
    let (package_name, version_spec, uri_variant) = resolve_source_spec(options)?;
    let source_paths = source_paths(options);
    let mut manager = RepositoryManager::new();
    for (index, path) in source_paths.iter().enumerate() {
        let repository =
            FilesystemPackageRepository::with_name(path, format!("transfer_repo_{index}"));
        manager.add_repository(Box::new(AsyncRepositoryAdapter::new(repository)));
    }

    let (source_package, source_path) = find_source_package(
        &manager,
        &package_name,
        version_spec.as_deref(),
        &source_paths,
    )
    .await?;
    let selected_variants = select_variants(
        &source_package,
        options
            .variant_indices
            .as_deref()
            .or(uri_variant.as_deref()),
    )?;
    let destination = destination_repository_path(options, &source_path)?;
    validate_destination(&destination, options.allow_empty)?;

    let destination_name = options.rename.as_deref().unwrap_or(&source_package.name);
    let source_version = source_package
        .version
        .as_ref()
        .map(|version| version.as_str())
        .ok_or_else(|| {
            RezCoreError::RequirementParse(
                "Package copy and move require a versioned source package".to_string(),
            )
        })?;
    let destination_version = options.reversion.as_deref().unwrap_or(source_version);
    let mut destination_descriptor = source_package.clone();
    destination_descriptor.name = destination_name.to_string();
    destination_descriptor.version = Some(
        Version::parse(destination_version)
            .map_err(|error| RezCoreError::RequirementParse(error.to_string()))?,
    );
    let destination_package = destination.join(PackageInstallLayout::package_base_relative_path(
        &destination_descriptor,
    )?);

    let same_canonical_path = fs::canonicalize(&source_path)
        .ok()
        .zip(fs::canonicalize(&destination_package).ok())
        .is_some_and(|(source, destination)| source == destination);
    if source_path == destination_package || same_canonical_path {
        return Err(RezCoreError::RequirementParse(
            "Cannot copy or move a package over itself".to_string(),
        ));
    }

    let mut overrides = HashMap::new();
    if let Some(name) = &options.rename {
        overrides.insert("name".to_string(), Value::String(name.clone()));
    }
    if let Some(version) = &options.reversion {
        overrides.insert("version".to_string(), Value::String(version.clone()));
    }

    if !options.dry_run {
        fs::create_dir_all(&destination).map_err(RezCoreError::Io)?;
    }
    let install_options = InstallVariantOptions {
        keep_timestamp: options.keep_timestamp,
        shallow: options.shallow,
        follow_symlinks: options.follow_symlinks,
        skip_payload: false,
    };
    rez_next_repository::package_repository::filesystem::install_package_variants(
        &rez_next_repository::package_repository::filesystem::InstallPackageVariantsRequest {
            source_package: source_package.clone(),
            source_package_dir: source_path.clone(),
            destination_repository: destination.clone(),
            destination_name: destination_name.to_string(),
            destination_version: destination_version.to_string(),
            variant_indices: selected_variants.clone(),
            dry_run: options.dry_run,
            overwrite: options.force || options.overwrite,
            overrides: (!overrides.is_empty()).then_some(overrides),
            options: install_options,
        },
    )?;

    if !options.dry_run && !options.keep_source {
        remove_source_payload(&source_package, &source_path, &selected_variants)?;
    }

    if options.verbose {
        let action = if options.keep_source {
            "Copied"
        } else {
            "Moved"
        };
        println!(
            "{action} {} variant(s) from {} to {}",
            selected_variants.len(),
            source_path.display(),
            destination_package.display()
        );
    }

    Ok(PackageTransferResult {
        destination_path: destination_package,
        variants_processed: selected_variants.len(),
    })
}

fn source_paths(options: &PackageTransferOptions) -> Vec<PathBuf> {
    if !options.source_paths.is_empty() {
        return options
            .source_paths
            .iter()
            .map(|path| expand_home_path(&path.to_string_lossy()))
            .collect();
    }

    let config = rez_next_common::config::RezCoreConfig::load();
    config
        .packages_path
        .iter()
        .map(|path| expand_home_path(path))
        .filter(|path| path.exists())
        .collect()
}

fn resolve_source_spec(
    options: &PackageTransferOptions,
) -> RezCoreResult<(String, Option<String>, Option<Vec<i32>>)> {
    if let Some(uri) = &options.variant_uri {
        let (package, version, index) = parse_variant_uri(uri)?;
        if options.source_package.is_some() {
            return Err(RezCoreError::RequirementParse(
                "Supply a package or --variant-uri, not both".to_string(),
            ));
        }
        return Ok((package, Some(version), Some(vec![index])));
    }

    let source = options
        .source_package
        .as_deref()
        .ok_or_else(|| RezCoreError::RequirementParse("Expected a source package".to_string()))?;
    let (name, version) = parse_package_spec(source)?;
    Ok((name, version, None))
}

fn destination_repository_path(
    options: &PackageTransferOptions,
    source_path: &Path,
) -> RezCoreResult<PathBuf> {
    if let Some(path) = &options.destination_path {
        return Ok(expand_home_path(&path.to_string_lossy()));
    }

    source_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            RezCoreError::RequirementParse(
                "--dest-path is required when the source repository cannot be inferred".to_string(),
            )
        })
}

fn validate_destination(path: &Path, allow_empty: bool) -> RezCoreResult<()> {
    if !path.exists() {
        return Ok(());
    }
    if !path.is_dir() {
        return Err(RezCoreError::Repository(format!(
            "Destination repository is not a directory: {}",
            path.display()
        )));
    }
    let is_empty = fs::read_dir(path)
        .map_err(RezCoreError::Io)?
        .next()
        .transpose()
        .map_err(RezCoreError::Io)?
        .is_none();
    if is_empty && !allow_empty {
        return Err(RezCoreError::Repository(format!(
            "Destination repository is empty: {}. Use --allow-empty to continue.",
            path.display()
        )));
    }
    Ok(())
}

async fn find_source_package(
    manager: &RepositoryManager,
    package_name: &str,
    version_spec: Option<&str>,
    source_paths: &[PathBuf],
) -> RezCoreResult<(Package, PathBuf)> {
    let packages = manager.find_packages(package_name).await?;
    if packages.is_empty() {
        return Err(RezCoreError::RequirementParse(format!(
            "Package '{}' not found in any source repository",
            package_name
        )));
    }

    let package = if let Some(version) = version_spec {
        packages
            .into_iter()
            .find(|package| {
                package
                    .version
                    .as_ref()
                    .is_some_and(|candidate| candidate.as_str() == version)
            })
            .ok_or_else(|| {
                RezCoreError::RequirementParse(format!(
                    "Package '{}-{}' not found",
                    package_name, version
                ))
            })?
    } else {
        packages
            .into_iter()
            .max_by(|left, right| left.version.cmp(&right.version))
            .unwrap()
    };

    let version = package
        .version
        .as_ref()
        .map(|version| version.as_str())
        .unwrap_or("unknown");
    for base in source_paths {
        let candidate = base.join(&package.name).join(version);
        if candidate.is_dir() {
            return Ok(((*package).clone(), candidate));
        }
        let alternate = base.join(format!("{}-{}", package.name, version));
        if alternate.is_dir() {
            return Ok(((*package).clone(), alternate));
        }
    }

    Err(RezCoreError::Repository(format!(
        "Package '{}' was indexed but its source directory was not found",
        package_name
    )))
}

fn select_variants(package: &Package, requested: Option<&[i32]>) -> RezCoreResult<Vec<usize>> {
    if package.variants.is_empty() {
        if requested.is_some_and(|indices| indices.iter().any(|index| *index != 0 && *index != -1))
        {
            return Err(RezCoreError::RequirementParse(
                "A non-variant package only accepts variant index 0 or -1".to_string(),
            ));
        }
        return Ok(vec![0]);
    }

    let raw = requested.map(ToOwned::to_owned).unwrap_or_else(|| {
        (0..package.variants.len())
            .map(|index| index as i32)
            .collect()
    });
    let mut indices = Vec::new();
    for index in raw {
        let resolved = if index < 0 {
            package.variants.len() as i32 + index
        } else {
            index
        };
        if resolved < 0 || resolved as usize >= package.variants.len() {
            return Err(RezCoreError::RequirementParse(format!(
                "Package '{}' has no variant {}",
                package.name, index
            )));
        }
        let resolved = resolved as usize;
        if !indices.contains(&resolved) {
            indices.push(resolved);
        }
    }
    Ok(indices)
}

fn remove_source_payload(
    package: &Package,
    source_path: &Path,
    selected_variants: &[usize],
) -> RezCoreResult<()> {
    if package.variants.is_empty() || selected_variants.len() == package.variants.len() {
        fs::remove_dir_all(source_path).map_err(RezCoreError::Io)?;
        return Ok(());
    }

    for index in selected_variants {
        let layout = PackageInstallLayout::for_variant(package, Some(*index))?;
        let variant_path = source_path.join(layout.variant_subpath());
        if variant_path.exists() {
            fs::remove_dir_all(variant_path).map_err(RezCoreError::Io)?;
        }
    }
    Ok(())
}

fn parse_package_spec(spec: &str) -> RezCoreResult<(String, Option<String>)> {
    if let Some(position) = spec.rfind('-') {
        let name = &spec[..position];
        let version = &spec[position + 1..];
        if version
            .chars()
            .next()
            .is_some_and(|value| value.is_ascii_digit())
        {
            return Ok((name.to_string(), Some(version.to_string())));
        }
    }
    Ok((spec.to_string(), None))
}

fn parse_variant_uri(uri: &str) -> RezCoreResult<(String, String, i32)> {
    let (package_uri, variant_suffix) = uri
        .rsplit_once("(variant ")
        .ok_or_else(|| RezCoreError::RequirementParse(format!("Invalid variant URI: {uri}")))?;
    let index = variant_suffix
        .strip_suffix(')')
        .ok_or_else(|| RezCoreError::RequirementParse(format!("Invalid variant URI: {uri}")))?
        .parse::<i32>()
        .map_err(|_| RezCoreError::RequirementParse(format!("Invalid variant URI: {uri}")))?;
    let package_spec = package_uri
        .rsplit_once('/')
        .map(|(_, value)| value)
        .unwrap_or(package_uri)
        .rsplit_once('-')
        .ok_or_else(|| RezCoreError::RequirementParse(format!("Invalid variant URI: {uri}")))?;
    Ok((
        package_spec.0.to_string(),
        package_spec.1.to_string(),
        index,
    ))
}
