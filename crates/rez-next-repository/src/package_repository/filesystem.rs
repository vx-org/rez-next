//! FilesystemPackageRepository implementation
//!
//! This module implements the PackageRepository trait for filesystem-based repositories.
//! It corresponds to the FilesystemPackageRepository class in rez's package_repository.py.

use super::{InstallVariantOptions, PackageRepository};
use crate::resources::{PackageFamilyResource, PackageResource, VariantResource};
use rez_next_common::RezCoreError;
use rez_next_package::{Package, PackageInstallLayout};
use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Filesystem repository type name
pub const FILESYSTEM_REPO_TYPE: &str = "filesystem";

// ── FilesystemPackageRepository ──────────────────────────────────────────────

/// Filesystem-based package repository
///
/// This corresponds to the FilesystemPackageRepository class in rez's
/// package_repository.py. It reads packages from a filesystem path.
#[derive(Debug, Clone)]
pub struct FilesystemPackageRepository {
    /// Repository location (filesystem path)
    location: PathBuf,
    /// Repository name (defaults to directory name)
    name: String,
    /// Repository priority (higher = more preferred)
    priority: i32,
    /// Whether this repository is read-only
    read_only: bool,
    /// Repository description
    description: Option<String>,
    /// Whether the repository has been initialized
    initialized: bool,
}

impl FilesystemPackageRepository {
    /// Create a new filesystem package repository
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        let location = path.as_ref().to_path_buf();
        let name = location
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        Self {
            location,
            name,
            priority: 0,
            read_only: false,
            description: None,
            initialized: false,
        }
    }

    /// Create with explicit name
    pub fn with_name<P: AsRef<Path>>(path: P, name: String) -> Self {
        let mut repo = Self::new(path);
        repo.name = name;
        repo
    }

    /// Set priority
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Set read-only flag
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Set description
    pub fn with_description(mut self, description: String) -> Self {
        self.description = Some(description);
        self
    }

    /// Get the repository location
    pub fn location(&self) -> &Path {
        &self.location
    }

    /// Get the repository name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if initialized
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn validate_component<'a>(value: &'a str, label: &str) -> Result<&'a str, RezCoreError> {
        let mut components = Path::new(value).components();
        let is_single_normal = matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none();
        if value.is_empty() || !is_single_normal {
            return Err(RezCoreError::Repository(format!(
                "Invalid {label} path component: {value:?}"
            )));
        }
        Ok(value)
    }

    fn require_version(version: Option<&str>) -> Result<&str, RezCoreError> {
        let version = version.ok_or_else(|| {
            RezCoreError::Repository(
                "Filesystem repository lifecycle operations require a package version".to_string(),
            )
        })?;
        Self::validate_component(version, "package version")
    }

    fn ensure_writable(&self, operation: &str) -> Result<(), RezCoreError> {
        if self.read_only {
            return Err(RezCoreError::Repository(format!(
                "Cannot {operation}: repository '{}' is read-only",
                self.location.display()
            )));
        }
        Ok(())
    }

    fn family_path(&self, name: &str) -> Result<PathBuf, RezCoreError> {
        Ok(self
            .location
            .join(Self::validate_component(name, "package name")?))
    }

    fn version_path(&self, name: &str, version: &str) -> Result<PathBuf, RezCoreError> {
        Ok(self
            .family_path(name)?
            .join(Self::validate_component(version, "package version")?))
    }

    fn ignore_path(&self, name: &str, version: &str) -> Result<PathBuf, RezCoreError> {
        Ok(self.family_path(name)?.join(format!(".ignore{version}")))
    }
}

impl PackageRepository for FilesystemPackageRepository {
    fn name() -> &'static str
    where
        Self: Sized,
    {
        FILESYSTEM_REPO_TYPE
    }

    fn get_package_family(
        &self,
        name: &str,
    ) -> Result<Option<PackageFamilyResource>, RezCoreError> {
        // Check if directory <location>/<name> exists
        let family_path = self.location.join(name);

        if !family_path.is_dir() {
            return Ok(None);
        }

        // Create PackageFamilyResource
        let family = PackageFamilyResource::new(
            name.to_string(),
            FILESYSTEM_REPO_TYPE.to_string(),
            self.location.to_string_lossy().to_string(),
        );

        Ok(Some(family))
    }

    fn iter_package_families(&self) -> Result<Vec<PackageFamilyResource>, RezCoreError> {
        let mut families = Vec::new();

        // Read the repository location directory
        let entries = fs::read_dir(&self.location).map_err(|e| {
            RezCoreError::Repository(format!(
                "Failed to read repository directory '{}': {}",
                self.location.display(),
                e
            ))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                RezCoreError::Repository(format!("Failed to read directory entry: {}", e))
            })?;

            let path = entry.path();

            // Only include directories (package families are directories)
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                // Skip hidden directories and special directories
                if name.starts_with('.') || name.starts_with('_') {
                    continue;
                }

                // Create a PackageFamilyResource
                let family = PackageFamilyResource::new(
                    name,
                    FILESYSTEM_REPO_TYPE.to_string(),
                    self.location.to_string_lossy().to_string(),
                );

                families.push(family);
            }
        }

        Ok(families)
    }

    fn iter_packages(
        &self,
        package_family: &PackageFamilyResource,
    ) -> Result<Vec<PackageResource>, RezCoreError> {
        let mut packages = Vec::new();

        // The family directory is <location>/<family_name>
        let family_path = self.location.join(&package_family.name);

        if !family_path.is_dir() {
            return Ok(packages);
        }

        // Read the family directory to find version directories
        let entries = fs::read_dir(&family_path).map_err(|e| {
            RezCoreError::Repository(format!(
                "Failed to read family directory '{}': {}",
                family_path.display(),
                e
            ))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                RezCoreError::Repository(format!("Failed to read directory entry: {}", e))
            })?;

            let path = entry.path();

            // Only include directories (versions are directories)
            if path.is_dir() {
                let version_str = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                // Skip hidden directories and special directories
                if version_str.starts_with('.') || version_str.starts_with('_') {
                    continue;
                }

                // Rez hides a version when a sibling `.ignore{version}` marker exists.
                if family_path.join(format!(".ignore{version_str}")).is_file() {
                    continue;
                }

                // Try to parse version string
                let version = rez_next_version::Version::parse(&version_str).ok();

                // Load the full package definition when the version directory carries
                // one, so dependency metadata (`requires`, `build_requires`,
                // `private_build_requires`, `tools`) is populated; upstream rez
                // resolves all of those from this file. A version directory with no
                // definition is still a valid, dependency-free package.
                //
                // A definition that exists but cannot be loaded is an error, not an
                // empty package: callers rely on `find_packages` failing closed so a
                // broken definition is never silently reported as "no packages".
                let mut pkg = match ["package.py", "package.yaml", "package.yml", "package.json"]
                    .iter()
                    .map(|name| path.join(name))
                    .find(|candidate| candidate.is_file())
                {
                    Some(definition_path) => {
                        let mut loaded =
                            rez_next_package::serialization::PackageSerializer::load_from_file(
                                &definition_path,
                            )
                            .map_err(|e| {
                                RezCoreError::Repository(format!(
                                    "Failed to load {}: {}",
                                    definition_path.display(),
                                    e
                                ))
                            })?;
                        // The serializer stores the path on the container, not on
                        // the package. Callers reach the package root through
                        // `Package::root()`, which derives it from `filepath`, so
                        // variant materialisation and payload paths depend on this
                        // being set here.
                        loaded.filepath = Some(definition_path.to_string_lossy().to_string());
                        loaded
                    }
                    None => rez_next_package::Package::new(package_family.name.clone()),
                };
                pkg.version = version;

                // Create PackageResource
                let resource = PackageResource::new(
                    pkg,
                    FILESYSTEM_REPO_TYPE.to_string(),
                    self.location.to_string_lossy().to_string(),
                );

                packages.push(resource);
            }
        }

        Ok(packages)
    }

    fn iter_variants(
        &self,
        package: &PackageResource,
    ) -> Result<Vec<VariantResource>, RezCoreError> {
        let mut variants = Vec::new();

        // Get version string
        let version_str = match &package.version() {
            Some(v) => v.as_str().to_string(),
            None => {
                // Package without version, no variants
                return Ok(variants);
            }
        };

        // The package directory is <location>/<family>/<version>
        let package_path = self.location.join(package.name()).join(&version_str);

        if !package_path.is_dir() {
            return Ok(variants);
        }

        // For now, create a single variant (index 0)
        // In a full implementation, would parse package.py for variants
        let variant = VariantResource::new(
            package.name().to_string(),
            Some(version_str),
            0, // index
            FILESYSTEM_REPO_TYPE.to_string(),
            self.location.to_string_lossy().to_string(),
        );

        variants.push(variant);

        Ok(variants)
    }

    fn ignore_package(
        &mut self,
        pkg_name: &str,
        pkg_version: Option<&str>,
        allow_missing: bool,
    ) -> Result<i32, RezCoreError> {
        self.ensure_writable("ignore package")?;
        let version = Self::require_version(pkg_version)?;
        let package_path = self.version_path(pkg_name, version)?;
        if !allow_missing && !package_path.is_dir() {
            return Ok(-1);
        }

        let family_path = self.family_path(pkg_name)?;
        fs::create_dir_all(&family_path).map_err(|e| {
            RezCoreError::Repository(format!(
                "Failed to create package family '{}': {e}",
                family_path.display()
            ))
        })?;
        let marker = self.ignore_path(pkg_name, version)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(_) => Ok(1),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(0),
            Err(e) => Err(RezCoreError::Repository(format!(
                "Failed to create ignore marker '{}': {e}",
                marker.display()
            ))),
        }
    }

    fn unignore_package(
        &mut self,
        pkg_name: &str,
        pkg_version: Option<&str>,
    ) -> Result<i32, RezCoreError> {
        self.ensure_writable("unignore package")?;
        let version = Self::require_version(pkg_version)?;
        let marker = self.ignore_path(pkg_name, version)?;
        let removed = match fs::remove_file(&marker) {
            Ok(()) => true,
            Err(e) if e.kind() == ErrorKind::NotFound => false,
            Err(e) => {
                return Err(RezCoreError::Repository(format!(
                    "Failed to remove ignore marker '{}': {e}",
                    marker.display()
                )));
            }
        };

        if !self.version_path(pkg_name, version)?.is_dir() {
            return Ok(-1);
        }
        Ok(i32::from(removed))
    }

    fn remove_package(
        &mut self,
        pkg_name: &str,
        pkg_version: Option<&str>,
    ) -> Result<bool, RezCoreError> {
        self.ensure_writable("remove package")?;
        let version = Self::require_version(pkg_version)?;
        if self.ignore_package(pkg_name, Some(version), false)? == -1 {
            return Ok(false);
        }

        let package_path = self.version_path(pkg_name, version)?;
        if let Err(e) = fs::remove_dir_all(&package_path) {
            return Err(RezCoreError::Repository(format!(
                "Failed to remove package payload '{}': {e}; the ignore marker was retained",
                package_path.display()
            )));
        }

        let marker = self.ignore_path(pkg_name, version)?;
        match fs::remove_file(&marker) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(true),
            Err(e) => Err(RezCoreError::Repository(format!(
                "Package payload was removed, but ignore marker '{}' could not be removed: {e}",
                marker.display()
            ))),
        }
    }

    fn remove_package_family(&mut self, pkg_name: &str, force: bool) -> Result<bool, RezCoreError> {
        self.ensure_writable("remove package family")?;
        let family_path = self.family_path(pkg_name)?;
        if !family_path.is_dir() {
            return Ok(false);
        }

        if !force {
            let has_packages = fs::read_dir(&family_path)
                .map_err(|e| {
                    RezCoreError::Repository(format!(
                        "Failed to inspect package family '{}': {e}",
                        family_path.display()
                    ))
                })?
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.path().is_dir() && !entry.file_name().to_string_lossy().starts_with('.')
                });
            if has_packages {
                return Err(RezCoreError::Repository(format!(
                    "Cannot remove non-empty package family {pkg_name:?} without force"
                )));
            }
        }

        fs::remove_dir_all(&family_path).map_err(|e| {
            RezCoreError::Repository(format!(
                "Failed to remove package family '{}': {e}",
                family_path.display()
            ))
        })?;
        Ok(true)
    }

    fn remove_ignored_since(
        &mut self,
        days: i32,
        dry_run: bool,
        verbose: bool,
    ) -> Result<i32, RezCoreError> {
        self.ensure_writable("remove ignored packages")?;
        if days < 0 {
            return Err(RezCoreError::Repository(
                "Ignored-package age must be zero or greater".to_string(),
            ));
        }

        let threshold = Duration::from_secs(days as u64 * 24 * 60 * 60);
        let now = SystemTime::now();
        let mut candidates = Vec::new();
        if !self.location.is_dir() {
            return Ok(0);
        }

        for family in fs::read_dir(&self.location).map_err(|e| {
            RezCoreError::Repository(format!(
                "Failed to inspect repository '{}': {e}",
                self.location.display()
            ))
        })? {
            let family = family.map_err(|e| RezCoreError::Repository(e.to_string()))?;
            if !family.path().is_dir() {
                continue;
            }
            let family_name = family.file_name().to_string_lossy().to_string();
            for entry in fs::read_dir(family.path()).map_err(|e| {
                RezCoreError::Repository(format!(
                    "Failed to inspect package family '{}': {e}",
                    family.path().display()
                ))
            })? {
                let entry = entry.map_err(|e| RezCoreError::Repository(e.to_string()))?;
                let filename = entry.file_name().to_string_lossy().to_string();
                let Some(version) = filename.strip_prefix(".ignore") else {
                    continue;
                };
                if version.is_empty() || !entry.path().is_file() {
                    continue;
                }
                let metadata = entry.metadata().map_err(|e| {
                    RezCoreError::Repository(format!(
                        "Failed to read ignore marker '{}': {e}",
                        entry.path().display()
                    ))
                })?;
                let timestamp = metadata
                    .created()
                    .or_else(|_| metadata.modified())
                    .map_err(|e| {
                        RezCoreError::Repository(format!(
                            "Failed to determine ignore marker age '{}': {e}",
                            entry.path().display()
                        ))
                    })?;
                if now.duration_since(timestamp).unwrap_or_default() >= threshold {
                    candidates.push((family_name.clone(), version.to_string()));
                }
            }
        }

        let mut removed = 0;
        for (name, version) in candidates {
            if verbose {
                tracing::info!(
                    package = %name,
                    version = %version,
                    dry_run,
                    "removing ignored package"
                );
            }
            if dry_run || self.remove_package(&name, Some(&version))? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn install_variant(
        &mut self,
        variant: &VariantResource,
        dry_run: bool,
        overrides: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<Option<PackageResource>, RezCoreError> {
        self.install_variant_with_options(
            variant,
            dry_run,
            overrides,
            InstallVariantOptions::default(),
        )
    }

    fn install_variant_with_options(
        &mut self,
        variant: &VariantResource,
        dry_run: bool,
        overrides: Option<HashMap<String, serde_json::Value>>,
        options: InstallVariantOptions,
    ) -> Result<Option<PackageResource>, RezCoreError> {
        Self::validate_component(&variant.name, "package name")?;
        let source_package_dir = variant_package_dir(variant)?;
        let source_definition = find_definition(&source_package_dir).ok_or_else(|| {
            RezCoreError::Repository(format!(
                "Variant source does not contain a package definition: {}",
                source_package_dir.display()
            ))
        })?;
        let mut package = Package::from_path(&source_package_dir).map_err(|error| {
            RezCoreError::Repository(format!(
                "Failed to load variant package definition '{}': {error}",
                source_definition.display()
            ))
        })?;
        let overrides = overrides.unwrap_or_default();
        apply_package_overrides(&mut package, &overrides)?;

        let selected_variant = if package.variants.is_empty() {
            None
        } else {
            Some(variant.index)
        };
        let layout = PackageInstallLayout::for_variant(&package, selected_variant)
            .map_err(|error| RezCoreError::Repository(error.to_string()))?;
        let target_package_dir = self.location.join(layout.package_relative_path());
        let target_variant_dir = self.location.join(layout.variant_relative_path());
        let source_variant_dir = variant_payload_dir(variant, &source_package_dir, &layout);

        if dry_run {
            return Ok(Some(PackageResource::new(
                package,
                FILESYSTEM_REPO_TYPE.to_string(),
                self.location.to_string_lossy().to_string(),
            )));
        }

        self.ensure_writable("install variant")?;
        if source_variant_dir == target_variant_dir {
            return Ok(Some(PackageResource::new(
                package,
                FILESYSTEM_REPO_TYPE.to_string(),
                self.location.to_string_lossy().to_string(),
            )));
        }

        fs::create_dir_all(&target_package_dir).map_err(|error| {
            RezCoreError::Repository(format!(
                "Failed to create package directory '{}': {error}",
                target_package_dir.display()
            ))
        })?;
        if !options.skip_payload
            && let Err(error) =
                copy_directory_contents(&source_variant_dir, &target_variant_dir, options)
        {
            return Err(RezCoreError::Repository(format!(
                "Failed to install variant payload '{}': {error}",
                source_variant_dir.display()
            )));
        }

        let definition_name = source_definition
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("package.py");
        let target_definition = target_package_dir.join(definition_name);
        if source_package_dir != target_package_dir {
            fs::copy(&source_definition, &target_definition).map_err(|error| {
                RezCoreError::Repository(format!(
                    "Failed to install package definition '{}': {error}",
                    source_definition.display()
                ))
            })?;
            if options.keep_timestamp {
                preserve_timestamps(&source_definition, &target_definition).map_err(|error| {
                    RezCoreError::Repository(format!(
                        "Failed to preserve package definition timestamp: {error}"
                    ))
                })?;
            }
        }
        if !overrides.is_empty() {
            fs::write(
                target_package_dir.join("package.py"),
                package.to_package_py(),
            )
            .map_err(|error| {
                RezCoreError::Repository(format!(
                    "Failed to write overridden package definition '{}': {error}",
                    target_package_dir.join("package.py").display()
                ))
            })?;
            if options.keep_timestamp {
                let target_definition = target_package_dir.join("package.py");
                preserve_timestamps(source_definition.as_path(), target_definition.as_path())
                    .map_err(|error| {
                        RezCoreError::Repository(format!(
                            "Failed to preserve overridden package definition timestamp: {error}"
                        ))
                    })?;
            }
        }

        Ok(Some(PackageResource::new(
            package,
            FILESYSTEM_REPO_TYPE.to_string(),
            self.location.to_string_lossy().to_string(),
        )))
    }

    fn get_parent_package_family(
        &self,
        package: &PackageResource,
    ) -> Result<PackageFamilyResource, RezCoreError> {
        Self::validate_component(package.name(), "package name")?;
        Ok(PackageFamilyResource::new(
            package.name().to_string(),
            FILESYSTEM_REPO_TYPE.to_string(),
            self.location.to_string_lossy().to_string(),
        ))
    }

    fn get_parent_package(
        &self,
        variant: &VariantResource,
    ) -> Result<PackageResource, RezCoreError> {
        Self::validate_component(&variant.name, "package name")?;
        let mut package = rez_next_package::Package::new(variant.name.clone());
        package.version = variant
            .version
            .as_deref()
            .map(rez_next_version::Version::parse)
            .transpose()
            .map_err(|e| RezCoreError::Repository(format!("Invalid package version: {e}")))?;
        Ok(PackageResource::new(
            package,
            FILESYSTEM_REPO_TYPE.to_string(),
            self.location.to_string_lossy().to_string(),
        ))
    }

    fn get_package_payload_path(
        &self,
        pkg_name: &str,
        pkg_version: Option<&str>,
    ) -> Result<Option<PathBuf>, RezCoreError> {
        let mut path = self.family_path(pkg_name)?;
        if let Some(version) = pkg_version {
            path.push(Self::validate_component(version, "package version")?);
        }
        Ok(Some(path))
    }

    fn repository_type(&self) -> &str {
        FILESYSTEM_REPO_TYPE
    }

    fn location(&self) -> &str {
        self.location.to_str().unwrap_or_default()
    }
}

/// Request for installing selected variants from an existing filesystem package.
#[derive(Debug, Clone)]
pub struct InstallPackageVariantsRequest {
    pub source_package: Package,
    pub source_package_dir: PathBuf,
    pub destination_repository: PathBuf,
    pub destination_name: String,
    pub destination_version: String,
    pub variant_indices: Vec<usize>,
    pub dry_run: bool,
    pub overwrite: bool,
    pub overrides: Option<HashMap<String, serde_json::Value>>,
    pub options: InstallVariantOptions,
}

/// Install selected variants from an existing filesystem package into a
/// destination repository through the canonical repository install hook.
pub fn install_package_variants(
    request: &InstallPackageVariantsRequest,
) -> Result<(), RezCoreError> {
    let mut destination_package = request.source_package.clone();
    destination_package.name = request.destination_name.clone();
    destination_package.version = Some(
        rez_next_version::Version::parse(&request.destination_version).map_err(|error| {
            RezCoreError::RequirementParse(format!(
                "Invalid destination package version '{}': {error}",
                request.destination_version
            ))
        })?,
    );
    let destination_package_dir = request.destination_repository.join(
        PackageInstallLayout::package_base_relative_path(&destination_package)
            .map_err(|error| RezCoreError::Repository(error.to_string()))?,
    );
    let same_canonical_path = fs::canonicalize(&request.source_package_dir)
        .ok()
        .zip(fs::canonicalize(&destination_package_dir).ok())
        .is_some_and(|(source, destination)| source == destination);
    if request.source_package_dir == destination_package_dir || same_canonical_path {
        return Err(RezCoreError::Repository(
            "Cannot install a package over itself".to_string(),
        ));
    }

    let source_location = request
        .source_package_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&request.source_package_dir);
    let mut repository = FilesystemPackageRepository::new(&request.destination_repository);
    let overrides = request
        .overrides
        .clone()
        .filter(|values| !values.is_empty());

    for variant_index in &request.variant_indices {
        let selected_variant = if request.source_package.variants.is_empty() {
            None
        } else {
            Some(*variant_index)
        };
        let layout = PackageInstallLayout::for_variant(&request.source_package, selected_variant)
            .map_err(|error| RezCoreError::Repository(error.to_string()))?;
        let target_variant_dir = destination_package_dir.join(layout.variant_subpath());

        if !request.dry_run && target_variant_dir.exists() {
            if !request.overwrite {
                return Err(RezCoreError::RequirementParse(format!(
                    "Package variant already exists at destination: {}. Use --overwrite or --force.",
                    target_variant_dir.display()
                )));
            }
            fs::remove_dir_all(&target_variant_dir).map_err(RezCoreError::Io)?;
        }

        let source_variant_dir = request.source_package_dir.join(layout.variant_subpath());
        let variant = VariantResource::new(
            request.source_package.name.clone(),
            request
                .source_package
                .version
                .as_ref()
                .map(|version| version.as_str().to_string()),
            *variant_index,
            FILESYSTEM_REPO_TYPE.to_string(),
            source_location.to_string_lossy().into_owned(),
        )
        .with_root(source_variant_dir.to_string_lossy().into_owned());

        repository.install_variant_with_options(
            &variant,
            request.dry_run,
            overrides.clone(),
            request.options,
        )?;
    }

    Ok(())
}

fn find_definition(package_dir: &Path) -> Option<PathBuf> {
    ["package.py", "package.yaml", "package.yml", "package.json"]
        .iter()
        .map(|name| package_dir.join(name))
        .find(|path| path.is_file())
}

fn variant_package_dir(variant: &VariantResource) -> Result<PathBuf, RezCoreError> {
    if let Some(root) = &variant.root {
        let root = PathBuf::from(root);
        if find_definition(&root).is_some() {
            return Ok(root);
        }

        let mut candidate = root.as_path();
        while let Some(parent) = candidate.parent() {
            if parent == candidate {
                break;
            }
            if find_definition(parent).is_some() {
                return Ok(parent.to_path_buf());
            }
            candidate = parent;
        }
        return Ok(root);
    }

    let version = variant.version.as_deref().ok_or_else(|| {
        RezCoreError::Repository("Cannot install an unversioned filesystem variant".to_string())
    })?;
    FilesystemPackageRepository::validate_component(version, "package version")?;
    Ok(PathBuf::from(&variant.repository_location)
        .join(FilesystemPackageRepository::validate_component(
            &variant.name,
            "package name",
        )?)
        .join(version))
}

fn variant_payload_dir(
    variant: &VariantResource,
    source_package_dir: &Path,
    layout: &PackageInstallLayout,
) -> PathBuf {
    if let Some(root) = &variant.root {
        let root = PathBuf::from(root);
        if root != source_package_dir {
            return root;
        }
    }
    source_package_dir.join(layout.variant_subpath())
}

fn copy_directory_contents(
    source: &Path,
    destination: &Path,
    options: InstallVariantOptions,
) -> Result<(), std::io::Error> {
    if !source.is_dir() {
        return Err(std::io::Error::new(
            ErrorKind::NotFound,
            format!("source directory does not exist: {}", source.display()),
        ));
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        let source_metadata = if file_type.is_symlink() && options.follow_symlinks {
            Some(fs::metadata(&source_path)?)
        } else {
            None
        };

        if file_type.is_symlink() && !options.follow_symlinks {
            copy_symlink(&source_path, &destination_path)?;
        } else if source_metadata
            .as_ref()
            .is_some_and(std::fs::Metadata::is_dir)
            || file_type.is_dir()
        {
            if options.shallow {
                create_directory_symlink(&source_path, &destination_path)?;
            } else {
                copy_directory_contents(&source_path, &destination_path, options)?;
            }
        } else if source_metadata
            .as_ref()
            .is_some_and(std::fs::Metadata::is_file)
            || file_type.is_file()
        {
            fs::copy(&source_path, &destination_path)?;
        }

        if options.keep_timestamp && !options.shallow && !file_type.is_symlink() {
            preserve_timestamps(&source_path, &destination_path)?;
        }
    }
    if options.keep_timestamp && !options.shallow {
        preserve_timestamps(source, destination)?;
    }
    Ok(())
}

fn preserve_timestamps(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    let metadata = fs::metadata(source)?;
    let accessed = filetime::FileTime::from_last_access_time(&metadata);
    let modified = filetime::FileTime::from_last_modification_time(&metadata);
    filetime::set_file_times(destination, accessed, modified)
}

fn copy_symlink(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    let target = fs::read_link(source)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, destination)
    }
    #[cfg(windows)]
    {
        let metadata = fs::metadata(source);
        if metadata.as_ref().is_ok_and(|value| value.is_dir()) {
            std::os::windows::fs::symlink_dir(target, destination)
        } else {
            std::os::windows::fs::symlink_file(target, destination)
        }
    }
}

fn create_directory_symlink(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, destination)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(source, destination)
    }
}

fn apply_package_overrides(
    package: &mut Package,
    overrides: &HashMap<String, serde_json::Value>,
) -> Result<(), RezCoreError> {
    for (name, value) in overrides {
        match name.as_str() {
            "name" => package.name = required_string_override(name, value)?,
            "version" => {
                package.version = Some(
                    rez_next_version::Version::parse(&required_string_override(name, value)?)
                        .map_err(|error| {
                            RezCoreError::Repository(format!(
                                "Invalid override for package attribute 'version': {error}"
                            ))
                        })?,
                );
            }
            "description" => package.description = optional_string_override(value)?,
            "authors" => package.authors = string_list_override(name, value)?,
            "requires" => package.requires = string_list_override(name, value)?,
            "build_requires" => package.build_requires = string_list_override(name, value)?,
            "private_build_requires" => {
                package.private_build_requires = string_list_override(name, value)?
            }
            "tools" => package.tools = string_list_override(name, value)?,
            "build_command" => package.build_command = optional_string_override(value)?,
            "build_system" => package.build_system = optional_string_override(value)?,
            "commands" => package.commands = optional_string_override(value)?,
            "pre_commands" => package.pre_commands = optional_string_override(value)?,
            "post_commands" => package.post_commands = optional_string_override(value)?,
            "pre_test_commands" => package.pre_test_commands = optional_string_override(value)?,
            "pre_build_commands" => package.pre_build_commands = optional_string_override(value)?,
            "help" => package.help = optional_string_override(value)?,
            "uuid" => package.uuid = optional_string_override(value)?,
            "relocatable" => package.relocatable = optional_bool_override(value)?,
            "cachable" => package.cachable = optional_bool_override(value)?,
            "revision" => package.revision = optional_string_override(value)?,
            "changelog" => package.changelog = optional_string_override(value)?,
            "release_message" => package.release_message = optional_string_override(value)?,
            "previous_revision" => package.previous_revision = optional_string_override(value)?,
            "vcs" => package.vcs = optional_string_override(value)?,
            "base" => package.base = optional_string_override(value)?,
            "requires_rez_version" => {
                package.requires_rez_version = optional_string_override(value)?
            }
            "has_plugins" => package.has_plugins = optional_bool_override(value)?,
            "hashed_variants" => package.hashed_variants = optional_bool_override(value)?,
            "preprocess" => package.preprocess = optional_string_override(value)?,
            unknown => {
                return Err(RezCoreError::Repository(format!(
                    "Unsupported package override attribute '{unknown}'"
                )));
            }
        }
    }
    Ok(())
}

fn required_string_override(name: &str, value: &serde_json::Value) -> Result<String, RezCoreError> {
    if value.as_str() == Some(super::REMOVE) {
        return Err(RezCoreError::Repository(format!(
            "Cannot remove package attribute '{name}'"
        )));
    }
    value.as_str().map(str::to_owned).ok_or_else(|| {
        RezCoreError::Repository(format!("Package override '{name}' must be a string"))
    })
}

fn optional_string_override(value: &serde_json::Value) -> Result<Option<String>, RezCoreError> {
    if value.as_str() == Some(super::REMOVE) || value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| {
            RezCoreError::Repository(
                "Package string override must be a string or remove sentinel".to_string(),
            )
        })
}

fn optional_bool_override(value: &serde_json::Value) -> Result<Option<bool>, RezCoreError> {
    if value.as_str() == Some(super::REMOVE) || value.is_null() {
        return Ok(None);
    }
    value.as_bool().map(Some).ok_or_else(|| {
        RezCoreError::Repository(
            "Package boolean override must be a boolean or remove sentinel".to_string(),
        )
    })
}

fn string_list_override(
    name: &str,
    value: &serde_json::Value,
) -> Result<Vec<String>, RezCoreError> {
    if value.as_str() == Some(super::REMOVE) || value.is_null() {
        return Ok(Vec::new());
    }
    value
        .as_array()
        .ok_or_else(|| {
            RezCoreError::Repository(format!("Package override '{name}' must be a string list"))
        })?
        .iter()
        .map(|item| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                RezCoreError::Repository(format!("Package override '{name}' must contain strings"))
            })
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rez_next_package::Package;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_filesystem_repository_create() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        assert_eq!(
            repo.name(),
            temp_dir.path().file_name().unwrap().to_str().unwrap()
        );
        assert_eq!(repo.repository_type(), "filesystem");
        assert!(!repo.is_initialized());
    }

    #[test]
    fn test_filesystem_repository_with_name() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::with_name(temp_dir.path(), "my_repo".to_string());

        assert_eq!(repo.name(), "my_repo");
    }

    #[test]
    fn test_filesystem_repository_with_priority() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path()).with_priority(10);

        // Need to add a way to get priority, or test via metadata
        assert_eq!(
            repo.name(),
            temp_dir.path().file_name().unwrap().to_str().unwrap()
        );
    }

    #[test]
    fn test_filesystem_repository_with_read_only() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path()).with_read_only(true);

        // Need to add a way to check read_only, or test via metadata
        assert!(!repo.is_initialized());
    }

    #[test]
    fn test_filesystem_repository_location() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        assert_eq!(repo.location(), temp_dir.path());
    }

    #[test]
    fn test_filesystem_repository_get_package_family_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        let result = repo.get_package_family("python");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_filesystem_repository_iter_package_families_empty() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        let result = repo.iter_package_families();
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_iter_package_families_with_families() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        // Create family directories
        let python_dir = temp_dir.path().join("python");
        let maya_dir = temp_dir.path().join("maya");
        fs::create_dir(&python_dir).unwrap();
        fs::create_dir(&maya_dir).unwrap();

        // Add a file (should not be included)
        let file_path = temp_dir.path().join("not_a_family.txt");
        fs::write(&file_path, "test").unwrap();

        let families = repo.iter_package_families().unwrap();
        assert_eq!(families.len(), 2);

        let names: Vec<&str> = families.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"python"));
        assert!(names.contains(&"maya"));
        assert!(!names.contains(&"not_a_family.txt"));
    }

    #[test]
    fn test_get_package_family_exists() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        // Create family directory
        let python_dir = temp_dir.path().join("python");
        fs::create_dir(&python_dir).unwrap();

        let family = repo.get_package_family("python").unwrap();
        assert!(family.is_some());

        let family = family.unwrap();
        assert_eq!(family.name, "python");
        assert_eq!(family.repository_type, "filesystem");
    }

    #[test]
    fn test_get_package_family_not_exists() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        let family = repo.get_package_family("nonexistent").unwrap();
        assert!(family.is_none());
    }

    #[test]
    fn test_iter_packages_with_versions() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        // Create family directory
        let python_dir = temp_dir.path().join("python");
        fs::create_dir(&python_dir).unwrap();

        // Create version directories
        let v390 = python_dir.join("3.9.0");
        let v3100 = python_dir.join("3.10.0");
        fs::create_dir(&v390).unwrap();
        fs::create_dir(&v3100).unwrap();

        // Create a PackageFamilyResource
        let family = PackageFamilyResource::new(
            "python".to_string(),
            "filesystem".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );

        let packages = repo.iter_packages(&family).unwrap();
        assert_eq!(packages.len(), 2);

        let versions: Vec<Option<&str>> = packages
            .iter()
            .map(|p| p.version().map(|v| v.as_str()))
            .collect();
        assert!(versions.contains(&Some("3.9.0")));
        assert!(versions.contains(&Some("3.10.0")));
    }

    #[test]
    fn test_iter_packages_empty() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        // Create empty family directory
        let python_dir = temp_dir.path().join("python");
        fs::create_dir(&python_dir).unwrap();

        let family = PackageFamilyResource::new(
            "python".to_string(),
            "filesystem".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );

        let packages = repo.iter_packages(&family).unwrap();
        assert!(packages.is_empty());
    }

    #[test]
    fn test_iter_variants() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        // Create package family directory
        let python_dir = temp_dir.path().join("python");
        fs::create_dir(&python_dir).unwrap();

        // Create version directory
        let version_dir = python_dir.join("3.9.0");
        fs::create_dir(&version_dir).unwrap();

        // Create a PackageResource with version
        let mut pkg = Package::new("python".to_string());
        pkg.version = Some(rez_next_version::Version::parse("3.9.0").unwrap());

        let resource = PackageResource::new(
            pkg,
            "filesystem".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );

        let variants = repo.iter_variants(&resource).unwrap();
        assert_eq!(variants.len(), 1);

        let variant = &variants[0];
        assert_eq!(variant.name, "python");
        assert_eq!(variant.index, 0);
    }

    #[test]
    fn test_iter_variants_no_version() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());

        // Create a PackageResource without version
        let pkg = rez_next_package::Package::new("python".to_string());
        let resource = PackageResource::new(
            pkg,
            "filesystem".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );

        let variants = repo.iter_variants(&resource).unwrap();
        assert!(variants.is_empty());
    }

    #[test]
    fn ignore_and_unignore_package_follow_rez_marker_contract() {
        let temp_dir = TempDir::new().unwrap();
        let family_dir = temp_dir.path().join("python");
        fs::create_dir_all(family_dir.join("3.12.0")).unwrap();
        let mut repo = FilesystemPackageRepository::new(temp_dir.path());

        assert_eq!(
            repo.ignore_package("python", Some("3.12.0"), false)
                .unwrap(),
            1
        );
        assert!(family_dir.join(".ignore3.12.0").is_file());
        assert!(
            repo.get_package("python", Some("3.12.0"))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repo.ignore_package("python", Some("3.12.0"), false)
                .unwrap(),
            0
        );

        assert_eq!(repo.unignore_package("python", Some("3.12.0")).unwrap(), 1);
        assert!(!family_dir.join(".ignore3.12.0").exists());
        assert!(
            repo.get_package("python", Some("3.12.0"))
                .unwrap()
                .is_some()
        );
        assert_eq!(repo.unignore_package("python", Some("3.12.0")).unwrap(), 0);
    }

    #[test]
    fn ignore_missing_package_requires_explicit_allow_missing() {
        let temp_dir = TempDir::new().unwrap();
        let mut repo = FilesystemPackageRepository::new(temp_dir.path());

        assert_eq!(
            repo.ignore_package("python", Some("3.12.0"), false)
                .unwrap(),
            -1
        );
        assert_eq!(
            repo.ignore_package("python", Some("3.12.0"), true).unwrap(),
            1
        );
        assert!(temp_dir.path().join("python/.ignore3.12.0").is_file());
    }

    #[test]
    fn remove_package_deletes_payload_and_marker() {
        let temp_dir = TempDir::new().unwrap();
        let package_dir = temp_dir.path().join("python/3.12.0");
        fs::create_dir_all(&package_dir).unwrap();
        fs::write(package_dir.join("package.py"), "name = 'python'").unwrap();
        let mut repo = FilesystemPackageRepository::new(temp_dir.path());

        assert!(repo.remove_package("python", Some("3.12.0")).unwrap());
        assert!(!package_dir.exists());
        assert!(!temp_dir.path().join("python/.ignore3.12.0").exists());
        assert!(!repo.remove_package("python", Some("3.12.0")).unwrap());
    }

    #[test]
    fn remove_package_family_requires_force_when_non_empty() {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir_all(temp_dir.path().join("python/3.12.0")).unwrap();
        let mut repo = FilesystemPackageRepository::new(temp_dir.path());

        assert!(repo.remove_package_family("python", false).is_err());
        assert!(repo.remove_package_family("python", true).unwrap());
        assert!(!temp_dir.path().join("python").exists());
        assert!(!repo.remove_package_family("python", true).unwrap());
    }

    #[test]
    fn remove_ignored_since_supports_dry_run_and_removal() {
        let temp_dir = TempDir::new().unwrap();
        let family_dir = temp_dir.path().join("python");
        fs::create_dir_all(family_dir.join("3.12.0")).unwrap();
        fs::write(family_dir.join(".ignore3.12.0"), "").unwrap();
        let mut repo = FilesystemPackageRepository::new(temp_dir.path());

        assert_eq!(repo.remove_ignored_since(0, true, false).unwrap(), 1);
        assert!(family_dir.join("3.12.0").exists());
        assert_eq!(repo.remove_ignored_since(0, false, false).unwrap(), 1);
        assert!(!family_dir.join("3.12.0").exists());
        assert!(!family_dir.join(".ignore3.12.0").exists());
    }

    #[test]
    fn payload_and_parent_resources_are_derived_from_repository_location() {
        let temp_dir = TempDir::new().unwrap();
        let repo = FilesystemPackageRepository::new(temp_dir.path());
        let mut package = Package::new("python".to_string());
        package.version = Some(rez_next_version::Version::parse("3.12.0").unwrap());
        let package = PackageResource::new(
            package,
            FILESYSTEM_REPO_TYPE.to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );
        let variant = VariantResource::new(
            "python".to_string(),
            Some("3.12.0".to_string()),
            0,
            FILESYSTEM_REPO_TYPE.to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );

        assert_eq!(
            repo.get_package_payload_path("python", Some("3.12.0"))
                .unwrap(),
            Some(temp_dir.path().join("python/3.12.0"))
        );
        assert_eq!(
            repo.get_parent_package_family(&package).unwrap().name,
            "python"
        );
        assert_eq!(repo.get_parent_package(&variant).unwrap().name(), "python");
    }

    #[test]
    fn destructive_operations_reject_read_only_and_unsafe_paths() {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir_all(temp_dir.path().join("python/3.12.0")).unwrap();
        let mut read_only = FilesystemPackageRepository::new(temp_dir.path()).with_read_only(true);
        assert!(read_only.remove_package("python", Some("3.12.0")).is_err());

        let mut repo = FilesystemPackageRepository::new(temp_dir.path());
        assert!(repo.remove_package("../outside", Some("3.12.0")).is_err());
        assert!(repo.remove_package("python", Some("../outside")).is_err());
        assert!(repo.remove_package("python", None).is_err());
    }

    #[test]
    fn install_variant_requires_source_definition() {
        let temp_dir = TempDir::new().unwrap();
        let mut repo = FilesystemPackageRepository::new(temp_dir.path());
        let variant = VariantResource::new(
            "python".to_string(),
            Some("3.12.0".to_string()),
            0,
            FILESYSTEM_REPO_TYPE.to_string(),
            temp_dir.path().to_string_lossy().to_string(),
        );

        assert!(repo.install_variant(&variant, false, None).is_err());
    }

    #[test]
    fn install_variant_copies_payload_and_definition() {
        let source = TempDir::new().unwrap();
        let destination = TempDir::new().unwrap();
        let source_package = source.path().join("python/3.12.0");
        fs::create_dir_all(source_package.join("bin")).unwrap();
        fs::write(
            source_package.join("package.py"),
            "name = 'python'\nversion = '3.12.0'\n",
        )
        .unwrap();
        fs::write(source_package.join("bin/python"), "python").unwrap();

        let variant = VariantResource::new(
            "python".to_string(),
            Some("3.12.0".to_string()),
            0,
            FILESYSTEM_REPO_TYPE.to_string(),
            source.path().to_string_lossy().to_string(),
        )
        .with_root(source_package.to_string_lossy().to_string());
        let mut repo = FilesystemPackageRepository::new(destination.path());

        let installed = repo.install_variant(&variant, false, None).unwrap();
        assert_eq!(installed.unwrap().name(), "python");
        assert_eq!(
            fs::read_to_string(destination.path().join("python/3.12.0/bin/python")).unwrap(),
            "python"
        );
        assert!(
            destination
                .path()
                .join("python/3.12.0/package.py")
                .is_file()
        );
    }

    #[test]
    fn install_variant_honours_dry_run_and_overrides() {
        let source = TempDir::new().unwrap();
        let destination = TempDir::new().unwrap();
        let source_package = source.path().join("python/3.12.0");
        fs::create_dir_all(&source_package).unwrap();
        fs::write(
            source_package.join("package.py"),
            "name = 'python'\nversion = '3.12.0'\n",
        )
        .unwrap();

        let variant = VariantResource::new(
            "python".to_string(),
            Some("3.12.0".to_string()),
            0,
            FILESYSTEM_REPO_TYPE.to_string(),
            source.path().to_string_lossy().to_string(),
        )
        .with_root(source_package.to_string_lossy().to_string());
        let mut repo = FilesystemPackageRepository::new(destination.path());

        let mut overrides = HashMap::new();
        overrides.insert(
            "name".to_string(),
            serde_json::Value::String("python-renamed".to_string()),
        );
        overrides.insert(
            "version".to_string(),
            serde_json::Value::String("4.0.0".to_string()),
        );
        overrides.insert(
            "description".to_string(),
            serde_json::Value::String("copied package".to_string()),
        );

        repo.install_variant(&variant, true, Some(overrides.clone()))
            .unwrap();
        assert!(!destination.path().join("python-renamed").exists());

        repo.install_variant(&variant, false, Some(overrides))
            .unwrap();
        let installed = destination.path().join("python-renamed/4.0.0");
        assert!(installed.join("package.py").is_file());
        let package = Package::from_path(&installed).unwrap();
        assert_eq!(package.name, "python-renamed");
        assert_eq!(package.version.unwrap().as_str(), "4.0.0");
        assert_eq!(package.description.as_deref(), Some("copied package"));
    }

    #[test]
    fn install_package_variants_uses_selected_payload_layout() {
        let source = TempDir::new().unwrap();
        let destination = TempDir::new().unwrap();
        let source_package = source.path().join("tool/1.0.0");
        fs::create_dir_all(source_package.join("python-3.11")).unwrap();
        fs::create_dir_all(source_package.join("python-3.12")).unwrap();
        fs::write(
            source_package.join("package.py"),
            "name = 'tool'\nversion = '1.0.0'\nvariants = [['python-3.11'], ['python-3.12']]\n",
        )
        .unwrap();
        fs::write(source_package.join("python-3.11/marker"), "old").unwrap();
        fs::write(source_package.join("python-3.12/marker"), "selected").unwrap();

        let package = Package::from_path(&source_package).unwrap();
        install_package_variants(&InstallPackageVariantsRequest {
            source_package: package,
            source_package_dir: source_package.clone(),
            destination_repository: destination.path().to_path_buf(),
            destination_name: "tool".to_string(),
            destination_version: "1.0.0".to_string(),
            variant_indices: vec![1],
            dry_run: false,
            overwrite: false,
            overrides: None,
            options: InstallVariantOptions::default(),
        })
        .unwrap();

        let installed = destination.path().join("tool/1.0.0");
        assert_eq!(
            fs::read_to_string(installed.join("python-3.12/marker")).unwrap(),
            "selected"
        );
        assert!(!installed.join("python-3.11/marker").exists());
        assert!(installed.join("package.py").is_file());
    }

    #[test]
    fn install_package_variants_overwrites_only_the_selected_variant() {
        let source = TempDir::new().unwrap();
        let destination = TempDir::new().unwrap();
        let source_package = source.path().join("tool/1.0.0");
        fs::create_dir_all(source_package.join("python-3.11")).unwrap();
        fs::create_dir_all(source_package.join("python-3.12")).unwrap();
        fs::write(
            source_package.join("package.py"),
            "name = 'tool'\nversion = '1.0.0'\nvariants = [['python-3.11'], ['python-3.12']]\n",
        )
        .unwrap();
        fs::write(source_package.join("python-3.11/marker"), "new-11").unwrap();
        fs::write(source_package.join("python-3.12/marker"), "new-12").unwrap();

        let installed_package = destination.path().join("tool/1.0.0");
        fs::create_dir_all(installed_package.join("python-3.11")).unwrap();
        fs::create_dir_all(installed_package.join("python-3.12")).unwrap();
        fs::write(installed_package.join("python-3.11/marker"), "keep-11").unwrap();
        fs::write(installed_package.join("python-3.12/marker"), "old-12").unwrap();
        fs::write(
            installed_package.join("package.py"),
            "name = 'tool'\nversion = '1.0.0'\nvariants = [['python-3.11'], ['python-3.12']]\n",
        )
        .unwrap();

        let package = Package::from_path(&source_package).unwrap();
        install_package_variants(&InstallPackageVariantsRequest {
            source_package: package,
            source_package_dir: source_package.clone(),
            destination_repository: destination.path().to_path_buf(),
            destination_name: "tool".to_string(),
            destination_version: "1.0.0".to_string(),
            variant_indices: vec![1],
            dry_run: false,
            overwrite: true,
            overrides: None,
            options: InstallVariantOptions::default(),
        })
        .unwrap();

        assert_eq!(
            fs::read_to_string(installed_package.join("python-3.11/marker")).unwrap(),
            "keep-11"
        );
        assert_eq!(
            fs::read_to_string(installed_package.join("python-3.12/marker")).unwrap(),
            "new-12"
        );
    }

    #[test]
    fn install_variant_preserves_file_and_definition_timestamps() {
        let source = TempDir::new().unwrap();
        let destination = TempDir::new().unwrap();
        let source_package = source.path().join("tool/1.0.0");
        fs::create_dir_all(source_package.join("bin")).unwrap();
        fs::write(
            source_package.join("package.py"),
            "name = 'tool'\nversion = '1.0.0'\n",
        )
        .unwrap();
        fs::write(source_package.join("bin/tool"), "tool").unwrap();
        let timestamp = filetime::FileTime::from_unix_time(1_234_567_890, 123);
        filetime::set_file_mtime(source_package.join("package.py"), timestamp).unwrap();
        filetime::set_file_mtime(source_package.join("bin/tool"), timestamp).unwrap();

        let variant = VariantResource::new(
            "tool".to_string(),
            Some("1.0.0".to_string()),
            0,
            FILESYSTEM_REPO_TYPE.to_string(),
            source.path().to_string_lossy().into_owned(),
        )
        .with_root(source_package.to_string_lossy().into_owned());
        let mut repository = FilesystemPackageRepository::new(destination.path());
        repository
            .install_variant_with_options(
                &variant,
                false,
                None,
                InstallVariantOptions {
                    keep_timestamp: true,
                    ..Default::default()
                },
            )
            .unwrap();

        let target_package = destination.path().join("tool/1.0.0");
        assert_eq!(
            filetime::FileTime::from_last_modification_time(
                &fs::metadata(target_package.join("package.py")).unwrap()
            )
            .seconds(),
            timestamp.seconds()
        );
        assert_eq!(
            filetime::FileTime::from_last_modification_time(
                &fs::metadata(target_package.join("bin/tool")).unwrap()
            )
            .seconds(),
            timestamp.seconds()
        );
    }
}
