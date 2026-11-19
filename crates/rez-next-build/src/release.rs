//! Complete release workflow implementation
//!
//! This module provides the core release workflow logic that orchestrates
//! VCS validation, package building, tag creation, and metadata generation.

use crate::vcs::{ReleaseVCS, VCSMetadata, detect_vcs};
use crate::{BuildManager, BuildOptions, BuildRequest};
use rez_next_common::{RezCoreConfig, RezCoreError};
use rez_next_context::{ContextConfig, EnvironmentManager, normalize_environment_paths};
use rez_next_package::serialization::{PackageFormat, PackageSerializer};
use rez_next_package::{
    Package, PackageInstallLayout, PackageRequirement, Requirement, TestCommand,
};
use rez_next_release_hook::{ReleaseHookError, ReleaseHookEvent};
use rez_next_repository::package_repository::FilesystemPackageRepository;
use rez_next_repository::package_repository::bridge::AsyncRepositoryAdapter;
use rez_next_repository::simple_repository::RepositoryManager;
use rez_next_solver::{DependencyResolver, SolverConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Release mode for the package release process
///
/// `Local` and `Release` differ only in which configured packages path the
/// package is installed into. `DryRun` writes nothing: it loads and validates
/// the package definition, resolves the install path, reads the repository
/// state, and returns early with [`ReleaseResult::success`] set to `true` and
/// the install path prefixed with `[dry-run] `. Nothing is built, installed, or
/// tagged.
///
/// "Writes nothing" is about mutation, not access — a dry run still *reads* the
/// VCS, calling `validate_repo_state()` and `get_metadata()`. It can therefore
/// report repository problems in [`ReleaseResult::errors`] while still setting
/// `success = true`; see
/// [`release_with_vcs`](ReleaseManager::release_with_vcs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReleaseMode {
    /// Normal release to `release_packages_path`.
    ///
    /// Requires `release_packages_path` to exist before a release starts.
    Release,
    /// Local release to `local_packages_path`
    Local,
    /// Dry run: validate but don't write
    DryRun,
}

impl ReleaseMode {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        match s {
            "local" => ReleaseMode::Local,
            "dry-run" | "dry_run" => ReleaseMode::DryRun,
            _ => ReleaseMode::Release,
        }
    }
}

/// Result of a release operation
///
/// A release reports failure through [`errors`](ReleaseResult::errors) rather
/// than through `Err`, so a caller gets every problem found in one pass instead
/// of only the first. Outside dry-run mode, [`success`](ReleaseResult::success)
/// is therefore `errors.is_empty()` and must be checked before anything else:
///
/// ```no_run
/// use rez_next_build::{ReleaseManager, ReleaseMode};
///
/// let manager = ReleaseManager::new(ReleaseMode::Local, true, true);
/// let result = manager.release(std::path::Path::new("."), None)?;
/// if !result.success {
///     for e in &result.errors {
///         eprintln!("release failed: {e}");
///     }
/// }
/// # Ok::<(), rez_next_common::RezCoreError>(())
/// ```
///
/// In [`ReleaseMode::DryRun`] that equivalence does not hold: a dry run sets
/// `success = true` unconditionally, so it can return `success == true`
/// together with a non-empty `errors` when repository validation failed. A dry
/// run is a check, so read `errors` there too — see
/// [`release_with_vcs`](ReleaseManager::release_with_vcs).
///
/// A returned `Err`([`RezCoreError`]) means the workflow itself could not run —
/// for example the release metadata could not be written. It never means
/// "the release was rejected"; a rejection is a successful call with a
/// non-empty `errors`.
#[derive(Debug, Clone, Default)]
pub struct ReleaseResult {
    /// `true` when the release completed with no errors.
    ///
    /// Equivalent to `errors.is_empty()` **except in
    /// [`ReleaseMode::DryRun`]**, which sets this to `true` unconditionally and
    /// can therefore report success alongside a non-empty `errors`.
    ///
    /// Always check this first: a rejected release returns `Ok` with
    /// `success == false`.
    pub success: bool,
    /// Package name read from the package definition. Empty when the definition
    /// could not be parsed.
    pub package_name: String,
    /// Package version read from the package definition. `"unknown"` when the
    /// definition parsed but carried no version, and `""` when the definition
    /// could not be read at all.
    pub version: String,
    /// Directory the package was installed into.
    ///
    /// In [`ReleaseMode::DryRun`] this is prefixed with `[dry-run] ` and nothing
    /// was written there.
    pub install_path: String,
    /// VCS metadata recorded for the release, present when a VCS was available
    /// and `get_metadata()` succeeded.
    ///
    /// The same metadata is written to `vcs_metadata.json` inside
    /// `install_path` (except in dry-run mode).
    pub vcs_metadata: Option<VCSMetadata>,
    /// Changelog generated by the VCS, present when a VCS was available and
    /// `get_changelog()` succeeded.
    pub changelog: Option<String>,
    /// Human-readable failures that made the release unsuccessful.
    ///
    /// Non-empty implies `success == false`, except in
    /// [`ReleaseMode::DryRun`], which reports success regardless. Entries are
    /// diagnostic messages meant for a log, not a stable machine-readable
    /// format — match on `success` and treat the text as display-only.
    pub errors: Vec<String>,
    /// Non-fatal notes: skipped steps, degraded VCS metadata, an existing tag
    /// that was kept, and so on. A release can succeed with warnings.
    pub warnings: Vec<String>,
}

/// Release manager that orchestrates the complete release workflow
#[derive(Debug, Clone)]
pub struct ReleaseManager {
    mode: ReleaseMode,
    skip_build: bool,
    skip_tests: bool, // now used in release() method
    /// Legacy opt-out that bypasses repository validation entirely.
    skip_vcs_validation: bool,
    /// Suppress only failures reported by a release VCS backend.
    skip_repo_errors: bool,
    /// How to treat an already-existing release tag.
    ///
    /// - `Some(false)`: hard-fail (the CLI default).
    /// - `Some(true)`: proceed, equivalent to `rez release --ignore-existing-tag`.
    /// - `None`: legacy behaviour — warn and continue.
    ignore_existing_tag: Option<bool>,
    /// Explicit release hooks. `None` uses the configured global hook list.
    release_hooks: Option<Vec<String>>,
    /// Reject a release when a newer version is already published.
    ensure_latest: bool,
    /// Optional zero-based variant selection. `None` releases every variant.
    variants: Option<Vec<usize>>,
}

impl ReleaseManager {
    /// Create a new ReleaseManager
    pub fn new(mode: ReleaseMode, skip_build: bool, skip_tests: bool) -> Self {
        Self {
            mode,
            skip_build,
            skip_tests,
            skip_vcs_validation: false,
            skip_repo_errors: false,
            ignore_existing_tag: None,
            release_hooks: None,
            ensure_latest: true,
            variants: None,
        }
    }

    /// Set whether to skip VCS validation
    pub fn set_skip_vcs_validation(&mut self, skip: bool) {
        self.skip_vcs_validation = skip;
    }

    /// Set whether dedicated release VCS errors should be downgraded to
    /// warnings while the workflow continues.
    pub fn set_skip_repo_errors(&mut self, skip: bool) {
        self.skip_repo_errors = skip;
    }

    /// Set how an existing release tag is treated.
    ///
    /// The default is `None`. See the tag policy table on
    /// [`release_with_vcs`](Self::release_with_vcs) for what each value does.
    ///
    /// `None` keeps the legacy warn-and-continue behaviour; `Some(false)` makes
    /// an existing tag a hard error; `Some(true)` allows re-releasing over it.
    ///
    /// Note that `Some(true)` and `None` behave identically today. The
    /// distinction is intent, and only `Some(true)` is guaranteed to keep
    /// allowing a re-release if the legacy default is ever tightened.
    pub fn set_ignore_existing_tag(&mut self, ignore_existing_tag: Option<bool>) {
        self.ignore_existing_tag = ignore_existing_tag;
    }

    /// Set the release hooks for this manager.
    ///
    /// Passing a list makes the release self-contained and is useful for
    /// embedders that already own their release configuration. Passing `None`
    /// restores the global `release_hooks` configuration.
    pub fn set_release_hooks(&mut self, release_hooks: Option<Vec<String>>) {
        self.release_hooks = release_hooks;
    }

    /// Set the zero-based variants to release.
    ///
    /// `None` releases every variant. The indices are validated against the
    /// loaded package before any build work starts.
    pub fn set_variants(&mut self, variants: Option<Vec<usize>>) {
        self.variants = variants;
    }

    /// Set whether a newer published version should reject this release.
    pub fn set_ensure_latest(&mut self, ensure_latest: bool) {
        self.ensure_latest = ensure_latest;
    }

    /// Execute the complete release workflow, detecting the VCS from
    /// `source_dir`.
    ///
    /// Equivalent to [`release_with_vcs`](Self::release_with_vcs) with
    /// `vcs: None`. See that method for the step order, the tag policy, and the
    /// result contract.
    pub fn release(
        &self,
        source_dir: &Path,
        message: Option<&str>,
    ) -> Result<ReleaseResult, RezCoreError> {
        self.release_with_vcs(source_dir, message, None)
    }

    /// Run the release workflow with a caller-supplied VCS instead of the one
    /// [`detect_vcs`] would find.
    ///
    /// This is the single implementation of the workflow; [`release`](Self::release)
    /// is a thin wrapper passing `None` so the VCS is detected. An external
    /// caller that owns its own VCS — a non-git provider, a wrapper around one,
    /// or one that needs injecting in a test — drives the release through this
    /// method.
    ///
    /// # Arguments
    ///
    /// * `source_dir` — directory holding `package.py` (or `package.yaml`). It is
    ///   also where [`detect_vcs`] looks for `.git` / `.hg` / `.svn` when `vcs`
    ///   is `None`.
    /// * `message` — release message used for the VCS tag. `None` produces
    ///   `Release {name}-{version}`. Ignored in [`ReleaseMode::DryRun`], where
    ///   it is echoed as a warning instead of tagging.
    /// * `vcs` — the VCS to drive the release with. `None` falls back to
    ///   [`detect_vcs`]; when that finds nothing the release proceeds without
    ///   one, recording a warning and skipping metadata, changelog, and the tag.
    ///   A supplied VCS is validated in step 2. Dedicated VCS failures can be
    ///   downgraded with
    ///   [`set_skip_repo_errors(true)`](Self::set_skip_repo_errors). The legacy
    ///   explicit validation bypass remains available through
    ///   [`set_skip_vcs_validation(true)`](Self::set_skip_vcs_validation), in
    ///   which case injecting one bypasses the repository-state check.
    ///
    /// # Steps
    ///
    /// 1. Load and validate the package definition
    /// 2. Detect and validate VCS repository state
    /// 3. Reject an already-released version (tag policy check, fails fast)
    /// 4. Build the package (including variants)
    /// 5. Run the package tests
    /// 6. Generate the changelog
    /// 7. Write release metadata
    /// 8. Install the package
    /// 9. Create the VCS tag
    ///
    /// [`ReleaseMode::DryRun`] returns between steps 2 and 3: it writes nothing,
    /// but it does *read* the VCS first, calling `validate_repo_state()` and
    /// `get_metadata()`.
    ///
    /// # Tag policy
    ///
    /// The default tag name is `{qualified_name}` (`{name}-{version}`). A
    /// configured `plugins.release_vcs.tag_name` may use `{name}`, `{version}`,
    /// and `{qualified_name}` placeholders. Step 3 only *inspects* it; step 9
    /// is what creates it, and only after every mutating step has succeeded. Where
    /// a VCS is in play, that ordering keeps the invariant **tag exists ⟺
    /// release succeeded**: a build or test failure leaves no tag behind, so the
    /// same version can be retried without an override. With no VCS there is no
    /// tag at all, and the release can still succeed.
    ///
    /// [`set_ignore_existing_tag`](Self::set_ignore_existing_tag) selects how an
    /// existing tag is treated:
    ///
    /// | Value | Behaviour when the tag already exists |
    /// | --- | --- |
    /// | `Some(false)` | **Hard failure.** Errors before anything is built or installed. |
    /// | `Some(true)` | Warn, install, report success. The existing tag is **kept**, never re-created or moved. |
    /// | `None` | Legacy: warn and continue, exactly as `Some(true)`. |
    ///
    /// For callers this means: a successful release guarantees the tag exists
    /// *only* if it did not already exist. It never means the tag was created by
    /// *this* run — check [`warnings`](ReleaseResult::warnings) for
    /// `Tag '<name>' already exists` to tell the two apart. A `Some(false)`
    /// release that reports success therefore always created the tag itself.
    ///
    /// # Result
    ///
    /// A rejected release is **not** an `Err`: it returns `Ok` with a non-empty
    /// [`errors`](ReleaseResult::errors) and `success == false`. Reserve `Err`
    /// for cases where the workflow itself could not run. Always check
    /// [`success`](ReleaseResult::success) rather than assuming `Ok` means
    /// success.
    ///
    /// ## Dry run is the exception
    ///
    /// In [`ReleaseMode::DryRun`] the `success == errors.is_empty()` equivalence
    /// that holds elsewhere **does not apply**. A dry run reports
    /// `success == true` unconditionally, so when step 2 rejects the repository
    /// state it returns `success == true` *together with* a non-empty `errors`:
    ///
    /// ```text
    /// success = true
    /// errors  = ["VCS validation failed: Repository is not clean"]
    /// ```
    ///
    /// This is reachable through the `vcs: None` path, where a failed
    /// `validate_repo_state()` is recorded but does not stop the run. (With an
    /// injected VCS the same failure returns immediately with
    /// `success == false`.)
    ///
    /// Since a dry run exists to surface problems before committing to a
    /// release, **check `errors` as well as `success`** when running one —
    /// treating that result as a pass would defeat its purpose.
    ///
    /// # Errors
    ///
    /// Returns `Err` only when the workflow cannot proceed at all — for example
    /// when the release metadata cannot be written. Step-level failures (a
    /// missing package definition, an unclean repository, a failed build or
    /// test, an existing tag under strict policy) are recorded in
    /// [`errors`](ReleaseResult::errors) and returned as `Ok`.
    pub fn release_with_vcs(
        &self,
        source_dir: &Path,
        message: Option<&str>,
        vcs: Option<std::sync::Arc<dyn ReleaseVCS + Send + Sync>>,
    ) -> Result<ReleaseResult, RezCoreError> {
        let mut result = ReleaseResult::default();

        // Step 1: Load package definition
        let mut package = self.load_package(source_dir, &mut result)?;
        if !result.errors.is_empty() {
            return Ok(result);
        }

        // Determine install path
        let install_path = self.get_install_path(&package)?;
        result.install_path = install_path.to_string_lossy().to_string();

        if self.mode == ReleaseMode::Release {
            let release_path = self.get_install_base();
            if release_path.as_os_str().is_empty() || !release_path.is_dir() {
                result.errors.push(format!(
                    "Release path does not exist or is not a directory: {}",
                    release_path.display()
                ));
                return Ok(result);
            }
        }

        // Step 2: VCS detection and validation. A caller-supplied VCS is still
        // validated, so injecting one cannot bypass the repository-state check.
        let vcs = match vcs {
            Some(vcs_impl) => {
                if !self.skip_vcs_validation
                    && let Err(e) = vcs_impl.validate_repo_state()
                {
                    if !self.skip_repo_error("validate repository state", &e, &mut result) {
                        result.errors.push(format!("VCS validation failed: {}", e));
                        return Ok(result);
                    }
                }
                match vcs_impl.get_metadata() {
                    Ok(metadata) => result.vcs_metadata = Some(metadata),
                    Err(e) => result
                        .warnings
                        .push(format!("Failed to get VCS metadata: {}", e)),
                }
                Some(vcs_impl)
            }
            None => {
                if !self.skip_vcs_validation {
                    self.validate_vcs(source_dir, &mut result)?
                } else {
                    None
                }
            }
        };

        // For dry-run mode, add prefix and return early
        if self.mode == ReleaseMode::DryRun {
            result.install_path = format!("[dry-run] {}", result.install_path);
            result.success = true;
            if let Some(msg) = message {
                result.warnings.push(format!("[dry-run] note: {}", msg));
            }
            return Ok(result);
        }

        self.populate_previous_release(&mut package, &mut result);

        if self.ensure_latest {
            self.reject_older_release(&package, &mut result);
        }
        if !result.errors.is_empty() {
            result.success = false;
            return Ok(result);
        }

        // Step 3: Reject an already-released version before anything is built
        // or installed. The tag check must run first: the build step copies
        // `package.py` into the install path, so checking afterwards would
        // overwrite an already-released version even when the release is
        // ultimately rejected.
        //
        // Invariant: tag exists ⟺ release succeeded. This step therefore only
        // checks the policy; the tag itself is created in step 9, after the
        // package has been installed.
        let mut tag_pre_existed = false;
        if let Some(ref vcs_impl) = vcs {
            tag_pre_existed = self.check_existing_tag(vcs_impl.as_ref(), &package, &mut result)?;
        }

        if !result.errors.is_empty() {
            result.success = false;
            return Ok(result);
        }

        if !self.run_release_hooks(
            ReleaseHookEvent::PreRelease,
            source_dir,
            &package,
            &install_path,
            message,
            None,
            None,
            &mut result,
        ) {
            result.success = false;
            return Ok(result);
        }

        // Step 4: Build the package (if not skipped)
        if !self.skip_build {
            self.build_package(source_dir, &package, &mut result)?;
        }

        // Step 5: Run tests (if not skipped)
        if self.skip_tests {
            result
                .warnings
                .push("Tests skipped (skip_tests=true)".to_string());
        } else {
            self.run_tests(&package, &install_path, &mut result)?;
        }

        if !result.errors.is_empty() {
            result.success = false;
            return Ok(result);
        }

        // Step 6: Generate changelog (if VCS is available)
        if let Some(ref vcs_impl) = vcs {
            self.generate_changelog(vcs_impl.as_ref(), &package, &mut result)?;
        }

        self.populate_release_metadata(&mut package, &vcs, message, &mut result);

        // Step 7: Write release metadata
        self.write_vcs_metadata(&install_path, &vcs, &mut result)?;

        // Step 8: Install package definition
        self.install_package_definition(source_dir, &install_path, &package, &mut result)?;

        let hook_changelog = result.changelog.clone().map(|value| vec![value]);
        if result.errors.is_empty()
            && !self.run_release_hooks(
                ReleaseHookEvent::PostRelease,
                source_dir,
                &package,
                &install_path,
                message,
                hook_changelog.as_deref(),
                None,
                &mut result,
            )
        {
            result.success = false;
            return Ok(result);
        }

        // Step 9: Create the VCS tag, and only now. Every mutating step has
        // happened above, so `errors.is_empty()` means the package really was
        // installed — a tag therefore never outlives a failed release. A tag
        // creation failure here is recoverable by re-running the release:
        // nothing has been overwritten, so the version is not wedged.
        //
        // When the tag already existed and step 3 let the release continue
        // anyway (`Some(true)` or the legacy `None`), there is nothing to
        // create: re-tagging would fail against the real providers and would
        // report an installed package as failed. The existing tag is kept.
        if let Some(ref vcs_impl) = vcs
            && result.errors.is_empty()
            && !tag_pre_existed
        {
            self.create_vcs_tag(vcs_impl.as_ref(), &package, message, &mut result)?;
        }

        result.success = result.errors.is_empty();
        Ok(result)
    }

    /// Load and validate package definition
    fn load_package(
        &self,
        source_dir: &Path,
        result: &mut ReleaseResult,
    ) -> Result<Package, RezCoreError> {
        let pkg_file = source_dir.join("package.py");
        let pkg_yaml = source_dir.join("package.yaml");

        let pkg_path = if pkg_file.exists() {
            &pkg_file
        } else if pkg_yaml.exists() {
            &pkg_yaml
        } else {
            result
                .errors
                .push("No package.py or package.yaml found".to_string());
            // Return Ok with default package - let release() decide based on result.errors
            return Ok(Package::new("".to_string()));
        };

        match PackageSerializer::load_from_file(pkg_path) {
            Ok(pkg) => {
                result.package_name = pkg.name.clone();
                result.version = pkg
                    .version
                    .as_ref()
                    .map(|v| v.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                // Validate package
                if pkg.name.is_empty() {
                    result.errors.push("Package name is empty".to_string());
                }
                if pkg.version.is_none() {
                    result.errors.push("Package version is not set".to_string());
                }

                Ok(pkg)
            }
            Err(e) => {
                let err_msg = format!("Failed to parse package: {}", e);
                result.errors.push(err_msg);
                // Return Ok with default package - let release() decide based on result.errors
                Ok(Package::new("".to_string()))
            }
        }
    }

    /// Get the install path for the package
    fn get_install_path(&self, package: &Package) -> Result<PathBuf, RezCoreError> {
        let package_relative_path = PackageInstallLayout::package_base_relative_path(package)?;
        Ok(self.get_install_base().join(package_relative_path))
    }

    /// Get the repository root used for the selected release mode.
    fn get_install_base(&self) -> PathBuf {
        let config = RezCoreConfig::load();
        let install_base = match self.mode {
            ReleaseMode::Local | ReleaseMode::DryRun => {
                PathBuf::from(expand_home(&config.local_packages_path))
            }
            ReleaseMode::Release => PathBuf::from(expand_home(&config.release_packages_path)),
        };
        install_base
    }

    /// Validate VCS repository state
    fn validate_vcs(
        &self,
        source_dir: &Path,
        result: &mut ReleaseResult,
    ) -> Result<Option<std::sync::Arc<dyn ReleaseVCS + Send + Sync>>, RezCoreError> {
        match detect_vcs(source_dir) {
            Err(error) => {
                if self.skip_repo_error("detect repository", &error, result) {
                    Ok(None)
                } else {
                    Err(error)
                }
            }
            Ok(Some(vcs_impl)) => {
                // Validate repo state
                if let Err(e) = vcs_impl.validate_repo_state() {
                    if !self.skip_repo_error("validate repository state", &e, result) {
                        result.errors.push(format!("VCS validation failed: {}", e));
                        return Ok(None);
                    }
                }

                // Get metadata
                match vcs_impl.get_metadata() {
                    Ok(metadata) => {
                        result.vcs_metadata = Some(metadata);
                    }
                    Err(e) => {
                        result
                            .warnings
                            .push(format!("Failed to get VCS metadata: {}", e));
                    }
                }

                Ok(Some(vcs_impl.into()))
            }
            Ok(None) => {
                result
                    .warnings
                    .push("No VCS detected in source directory".to_string());
                Ok(None)
            }
        }
    }

    /// Downgrade a dedicated VCS failure when the caller requested the
    /// upstream `skip_repo_errors` policy.
    fn skip_repo_error(
        &self,
        operation: &str,
        error: &RezCoreError,
        result: &mut ReleaseResult,
    ) -> bool {
        if self.skip_repo_errors && matches!(error, RezCoreError::ReleaseVCSError(_)) {
            result
                .warnings
                .push(format!("Skipped VCS operation '{operation}': {error}"));
            true
        } else {
            false
        }
    }

    /// Build the package (including variants)
    fn build_package(
        &self,
        source_dir: &Path,
        package: &Package,
        result: &mut ReleaseResult,
    ) -> Result<(), RezCoreError> {
        let variant_indices = self.release_variant_indices(package)?;
        let install_base = self.get_install_base();
        let runtime = tokio::runtime::Runtime::new().map_err(|error| {
            RezCoreError::BuildError(format!("Failed to create release build runtime: {error}"))
        })?;
        let mut build_manager = BuildManager::new();

        if package.variants.is_empty() {
            result
                .warnings
                .push("Package has no variants, building the base package".to_string());
        } else {
            result.warnings.push(format!(
                "Building {} selected variant(s)",
                variant_indices.len()
            ));
        }

        for variant_index in variant_indices {
            let variant_requires = variant_index.map(|index| package.variants[index].clone());
            let context = match resolve_build_context(package, variant_requires.as_deref()) {
                Ok(context) => context,
                Err(error) => {
                    result.errors.push(format!(
                        "Failed to resolve build context for variant {}: {}",
                        variant_index.map_or_else(|| "none".to_string(), |index| index.to_string()),
                        error
                    ));
                    return Ok(());
                }
            };
            let request = BuildRequest {
                package: package.clone(),
                context,
                source_dir: source_dir.to_path_buf(),
                variant_index,
                variant_requires,
                options: BuildOptions {
                    force_rebuild: false,
                    skip_tests: self.skip_tests,
                    release_mode: true,
                    build_args: Vec::new(),
                    env_vars: std::collections::HashMap::new(),
                },
                install_path: Some(install_base.clone()),
            };

            let build_ids = match runtime.block_on(build_manager.start_build(request)) {
                Ok(build_ids) => build_ids,
                Err(error) => {
                    result.errors.push(format!(
                        "Failed to start build for variant {}: {}",
                        variant_index.map_or_else(|| "none".to_string(), |index| index.to_string()),
                        error
                    ));
                    return Ok(());
                }
            };
            for build_id in build_ids {
                let build_result = match runtime.block_on(build_manager.wait_for_build(&build_id)) {
                    Ok(build_result) => build_result,
                    Err(error) => {
                        result.errors.push(format!(
                            "Failed to collect build result for variant {}: {}",
                            variant_index
                                .map_or_else(|| "none".to_string(), |index| index.to_string()),
                            error
                        ));
                        return Ok(());
                    }
                };
                if !build_result.success {
                    result.errors.push(format!(
                        "Build failed for variant {}: {}",
                        variant_index.map_or_else(|| "none".to_string(), |index| index.to_string()),
                        build_result.errors
                    ));
                    return Ok(());
                }
            }
        }

        Ok(())
    }

    fn release_variant_indices(
        &self,
        package: &Package,
    ) -> Result<Vec<Option<usize>>, RezCoreError> {
        if package.variants.is_empty() {
            if self
                .variants
                .as_ref()
                .is_some_and(|indices| !indices.is_empty())
            {
                return Err(RezCoreError::BuildError(format!(
                    "Package '{}' has no variants",
                    package.name
                )));
            }
            return Ok(vec![None]);
        }

        let indices = self
            .variants
            .clone()
            .unwrap_or_else(|| (0..package.variants.len()).collect());
        indices
            .into_iter()
            .map(|index| {
                if index >= package.variants.len() {
                    Err(RezCoreError::BuildError(format!(
                        "Variant index {index} is out of range for package '{}'",
                        package.name
                    )))
                } else {
                    Ok(Some(index))
                }
            })
            .collect()
    }

    /// Check whether an already-released version is being released again.
    ///
    /// This is the policy gate that runs *before* anything is built or
    /// installed. It only inspects state, never mutates it: the tag itself is
    /// created by [`Self::create_vcs_tag`] once the release has succeeded. That
    /// split keeps the invariant **tag exists ⟺ release succeeded** — a tag
    /// created before the build would survive a build/test failure and leave a
    /// version permanently wedged.
    ///
    /// Returns `true` when the tag already existed *and* the policy let the
    /// release continue anyway. Step 9 uses that to skip `create_tag`: the real
    /// providers create tags with `force = false` and do not pre-check for
    /// existence, so re-tagging an existing name would fail and turn an
    /// installed release into a reported failure. Forcing the tag is not an
    /// option — it would move an already-released version's tag onto a new
    /// commit, which is exactly the provenance tampering this policy prevents.
    fn check_existing_tag(
        &self,
        vcs: &dyn ReleaseVCS,
        package: &Package,
        result: &mut ReleaseResult,
    ) -> Result<bool, RezCoreError> {
        let tag_name = Self::release_tag_name(package);
        let mut tag_pre_existed = false;

        match vcs.tag_exists(&tag_name) {
            Ok(true) => match self.ignore_existing_tag {
                // Explicit opt-in: re-release over the existing tag. The tag
                // keeps pointing at the original commit — moving it is what
                // `--ignore-existing-tag` deliberately does not do.
                Some(true) => {
                    tag_pre_existed = true;
                    result
                        .warnings
                        .push(format!("Tag '{}' already exists", tag_name));
                }
                // Strict: an existing tag means this version was already
                // released. Overwriting the install while the tag stays on the
                // old commit would leave the tag and the installed
                // `vcs_metadata.json` disagreeing about the source commit.
                Some(false) => {
                    result.errors.push(format!(
                        "Release tag '{}' already exists. Use --ignore-existing-tag to override.",
                        tag_name
                    ));
                }
                // Legacy: warn and continue (preserves pre-existing behaviour).
                None => {
                    tag_pre_existed = true;
                    result
                        .warnings
                        .push(format!("Tag '{}' already exists", tag_name));
                }
            },
            Ok(false) => {}
            Err(e) => {
                result
                    .warnings
                    .push(format!("Failed to check tag existence: {}", e));
            }
        }

        Ok(tag_pre_existed)
    }

    /// Build the VCS tag name for a package version.
    fn release_tag_name(package: &Package) -> String {
        let tag_format = RezCoreConfig::load()
            .get_field("plugins.release_vcs.tag_name")
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "{qualified_name}".to_string());

        format_release_tag(package, &tag_format)
    }

    /// Create the VCS tag for a successfully released version.
    ///
    /// Called only after the package has been built, tested and installed, so
    /// a failure in any of those steps leaves no tag behind.
    fn create_vcs_tag(
        &self,
        vcs: &dyn ReleaseVCS,
        package: &Package,
        message: Option<&str>,
        result: &mut ReleaseResult,
    ) -> Result<(), RezCoreError> {
        let tag_name = Self::release_tag_name(package);
        let default_message = format!("Release {}", tag_name);
        let tag_message = message.unwrap_or(&default_message);

        match vcs.create_tag(&tag_name, tag_message) {
            Ok(_) => {
                result
                    .warnings
                    .push(format!("Created VCS tag: {}", tag_name));
            }
            Err(e) => {
                if !self.skip_repo_error("create release tag", &e, result) {
                    result
                        .errors
                        .push(format!("Failed to create VCS tag: {}", e));
                }
            }
        }

        Ok(())
    }

    /// Run the configured hooks for one release lifecycle event.
    ///
    /// Hook construction failures and ordinary hook errors are warnings so an
    /// optional notification hook cannot make a release unsafe to retry. A
    /// cancellation is a deliberate release rejection and is recorded as an
    /// error so the caller receives the normal `ReleaseResult` failure contract.
    #[allow(clippy::too_many_arguments)]
    fn run_release_hooks(
        &self,
        event: ReleaseHookEvent,
        source_dir: &Path,
        package: &Package,
        install_path: &Path,
        release_message: Option<&str>,
        changelog: Option<&[String]>,
        variants: Option<&[usize]>,
        result: &mut ReleaseResult,
    ) -> bool {
        let hook_names = self
            .release_hooks
            .clone()
            .unwrap_or_else(|| RezCoreConfig::load().release_hooks);
        if hook_names.is_empty() {
            return true;
        }

        rez_next_release_hook::init();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default();
        let previous_version = package
            .previous_version
            .as_ref()
            .map(|version| version.as_str());
        let previous_revision = package.previous_revision.as_deref();
        let hook_names: Vec<&str> = hook_names.iter().map(String::as_str).collect();
        let hooks =
            rez_next_release_hook::create_release_hooks(&hook_names, source_dir.to_path_buf());

        for hook in hooks {
            let hook_result = match event {
                ReleaseHookEvent::PreBuild => hook.pre_build(
                    &user,
                    install_path,
                    variants,
                    release_message,
                    changelog,
                    previous_version,
                    previous_revision,
                ),
                ReleaseHookEvent::PreRelease => hook.pre_release(
                    &user,
                    install_path,
                    variants,
                    release_message,
                    changelog,
                    previous_version,
                    previous_revision,
                ),
                ReleaseHookEvent::PostRelease => hook.post_release(
                    &user,
                    install_path,
                    &Self::release_variant_names(package),
                    release_message,
                    changelog,
                    previous_version,
                    previous_revision,
                ),
            };

            match hook_result {
                Ok(()) => {}
                Err(ReleaseHookError::Cancelled(message)) => {
                    result.errors.push(format!(
                        "Release hook '{}' cancelled release: {message}",
                        event.label()
                    ));
                    return false;
                }
                Err(ReleaseHookError::Error(message)) => {
                    result.warnings.push(format!(
                        "Release hook '{}' failed: {message}",
                        event.label()
                    ));
                }
            }
        }

        true
    }

    fn release_variant_names(package: &Package) -> Vec<String> {
        if package.variants.is_empty() {
            vec!["0".to_string()]
        } else {
            (0..package.variants.len())
                .map(|index| index.to_string())
                .collect()
        }
    }

    /// Generate changelog from VCS
    fn generate_changelog(
        &self,
        vcs: &dyn ReleaseVCS,
        package: &Package,
        result: &mut ReleaseResult,
    ) -> Result<(), RezCoreError> {
        match vcs.get_changelog(package.previous_revision.as_deref(), None) {
            Ok(changelog) => {
                result.changelog = Some(changelog);
            }
            Err(e) => {
                result
                    .warnings
                    .push(format!("Failed to generate changelog: {}", e));
            }
        }
        Ok(())
    }

    /// Populate the previous release fields before pre-release hooks run.
    fn populate_previous_release(&self, package: &mut Package, result: &mut ReleaseResult) {
        if package.previous_version.is_some() || package.version.is_none() {
            return;
        }

        match self.find_previous_release(package) {
            Ok(Some(previous)) => {
                package.previous_version = previous.version.clone();
                package.previous_revision = previous.revision.clone();
            }
            Ok(None) => {}
            Err(error) => result
                .warnings
                .push(format!("Failed to find previous release: {error}")),
        }
    }

    /// Populate release provenance on the package definition that is installed.
    fn populate_release_metadata(
        &self,
        package: &mut Package,
        vcs: &Option<Arc<dyn ReleaseVCS + Send + Sync>>,
        release_message: Option<&str>,
        result: &mut ReleaseResult,
    ) {
        package.timestamp = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
        );
        package.release_message = release_message.map(str::to_owned);
        package.changelog = result.changelog.as_deref().map(|changelog| {
            truncate_changelog(changelog, RezCoreConfig::load().max_package_changelog_chars)
        });

        if let Some(vcs_impl) = vcs {
            package.vcs = Some(vcs_impl.get_type_name().to_string());
            match vcs_impl.get_current_revision() {
                Ok(revision) => package.revision = Some(revision.revision_id),
                Err(error) => result
                    .warnings
                    .push(format!("Failed to get VCS revision: {error}")),
            }
        } else {
            package.vcs = None;
            package.revision = None;
        }
    }

    /// Reject a release when a newer package version is already available.
    fn reject_older_release(&self, package: &Package, result: &mut ReleaseResult) {
        let Some(current_version) = package.version.as_ref() else {
            return;
        };

        match self.find_latest_release(package) {
            Ok(Some(latest))
                if latest
                    .version
                    .as_ref()
                    .is_some_and(|version| version > current_version) =>
            {
                result.errors.push(format!(
                    "Newer published version exists ({} > {}). Set ensure_latest=false to bypass.",
                    latest
                        .version
                        .as_ref()
                        .map_or_else(|| "unknown".to_string(), ToString::to_string),
                    current_version
                ));
            }
            Ok(_) => {}
            Err(error) => result
                .warnings
                .push(format!("Failed to check for newer releases: {error}")),
        }
    }

    /// Find the newest released package version older than the package being built.
    fn find_previous_release(
        &self,
        package: &Package,
    ) -> Result<Option<Arc<Package>>, RezCoreError> {
        let Some(current_version) = package.version.as_ref() else {
            return Ok(None);
        };

        Ok(self
            .find_releases(package)?
            .into_iter()
            .filter(|candidate| {
                candidate
                    .version
                    .as_ref()
                    .is_some_and(|version| version < current_version)
            })
            .max_by(|left, right| left.version.cmp(&right.version)))
    }

    /// Find the newest package version visible from the configured repositories.
    fn find_latest_release(&self, package: &Package) -> Result<Option<Arc<Package>>, RezCoreError> {
        Ok(self
            .find_releases(package)?
            .into_iter()
            .filter(|candidate| candidate.version.is_some())
            .max_by(|left, right| left.version.cmp(&right.version)))
    }

    fn find_releases(&self, package: &Package) -> Result<Vec<Arc<Package>>, RezCoreError> {
        let mut repository_manager = RepositoryManager::new();

        let config = RezCoreConfig::load();
        let mut search_paths = vec![self.get_install_base()];
        search_paths.extend(
            config
                .packages_path
                .iter()
                .map(|search_path| PathBuf::from(expand_home(search_path))),
        );
        search_paths.dedup();
        for (index, path) in search_paths.into_iter().enumerate() {
            if path.exists() {
                let repository =
                    FilesystemPackageRepository::with_name(path, format!("release_repo_{index}"));
                repository_manager
                    .add_repository(Box::new(AsyncRepositoryAdapter::new(repository)));
            }
        }

        let runtime = tokio::runtime::Runtime::new().map_err(|error| {
            RezCoreError::BuildError(format!(
                "Failed to create release metadata runtime: {error}"
            ))
        })?;
        runtime.block_on(repository_manager.find_packages(&package.name))
    }

    /// Write the VCS metadata sidecar for an installed package.
    fn write_vcs_metadata(
        &self,
        install_path: &Path,
        vcs: &Option<std::sync::Arc<dyn ReleaseVCS + Send + Sync>>,
        result: &mut ReleaseResult,
    ) -> Result<(), RezCoreError> {
        // Get VCS metadata if VCS is available
        if let Some(vcs_impl) = vcs {
            match vcs_impl.get_metadata() {
                Ok(metadata) => {
                    // Write VCS metadata to a separate JSON file
                    let metadata_path = install_path.join("vcs_metadata.json");
                    match serde_json::to_string_pretty(&metadata) {
                        Ok(json_str) => match fs::write(&metadata_path, json_str) {
                            Ok(_) => {
                                result.vcs_metadata = Some(metadata);
                            }
                            Err(e) => {
                                result
                                    .warnings
                                    .push(format!("Failed to write VCS metadata: {}", e));
                            }
                        },
                        Err(e) => {
                            result
                                .warnings
                                .push(format!("Failed to serialize VCS metadata: {}", e));
                        }
                    }
                }
                Err(e) => {
                    result
                        .warnings
                        .push(format!("Failed to get VCS metadata: {}", e));
                }
            }
        } else {
            // No VCS detected, skip metadata writing
            result
                .warnings
                .push("No VCS detected, skipping metadata writing".to_string());
        }
        Ok(())
    }

    /// Install package definition to the install path
    fn install_package_definition(
        &self,
        source_dir: &Path,
        install_path: &Path,
        package: &Package,
        result: &mut ReleaseResult,
    ) -> Result<(), RezCoreError> {
        let pkg_file = source_dir.join("package.py");
        let pkg_yaml = source_dir.join("package.yaml");

        let (src, dest_name) = if pkg_file.exists() {
            (&pkg_file, "package.py")
        } else if pkg_yaml.exists() {
            (&pkg_yaml, "package.yaml")
        } else {
            return Ok(());
        };

        let dest = install_path.join(dest_name);
        if dest_name == "package.py" {
            let content = fs::read_to_string(src).map_err(|error| {
                RezCoreError::BuildError(format!(
                    "Failed to read package definition {}: {error}",
                    src.display()
                ))
            })?;
            let content = append_python_release_metadata(&content, package)?;
            fs::write(&dest, content).map_err(|error| {
                result.errors.push(format!(
                    "Failed to install package definition {}: {error}",
                    dest.display()
                ));
                RezCoreError::BuildError(error.to_string())
            })?;
        } else {
            PackageSerializer::save_to_file(package, &dest, PackageFormat::Yaml).map_err(
                |error| {
                    result.errors.push(format!(
                        "Failed to install package definition {}: {error}",
                        dest.display()
                    ));
                    error
                },
            )?;
        }

        Ok(())
    }

    /// Run package tests configured for the pre-release phase.
    fn run_tests(
        &self,
        package: &Package,
        install_path: &Path,
        result: &mut ReleaseResult,
    ) -> Result<(), RezCoreError> {
        let mut tests: Vec<_> = package
            .tests
            .iter()
            .filter(|(_, definition)| definition.run_on.iter().any(|tag| tag == "pre_release"))
            .collect();
        tests.sort_by_key(|(name, _)| *name);

        if tests.is_empty() {
            result
                .warnings
                .push("No pre_release package tests defined".to_string());
            return Ok(());
        }

        let mut base_environment = std::env::vars().collect();
        normalize_environment_paths(&mut base_environment);
        let manager = EnvironmentManager::with_base_environment(
            ContextConfig {
                inherit_parent_env: true,
                ..ContextConfig::default()
            },
            base_environment,
        );
        let runtime = tokio::runtime::Runtime::new().map_err(|error| {
            RezCoreError::BuildError(format!("Failed to create test runtime: {error}"))
        })?;
        let definition_name = package
            .filepath
            .as_deref()
            .and_then(|path| Path::new(path).file_name())
            .unwrap_or_else(|| std::ffi::OsStr::new("package.py"));

        for (name, definition) in tests {
            let mut test_package = package.clone();
            test_package.filepath = Some(
                install_path
                    .join(definition_name)
                    .to_string_lossy()
                    .into_owned(),
            );
            test_package
                .requires
                .extend(definition.requires.iter().cloned());
            let environment = runtime.block_on(manager.generate_environment(&[test_package]))?;
            match Self::execute_test_definition(definition, install_path, &environment) {
                Ok(output) => {
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        result.errors.push(format!(
                            "Package test '{name}' failed:\nCommand: {}\nError: {}",
                            definition.command.as_string(),
                            stderr
                        ));
                        break;
                    }
                }
                Err(e) => {
                    result.errors.push(format!(
                        "Failed to execute package test '{name}': {}\nCommand: {}",
                        e,
                        definition.command.as_string()
                    ));
                    break;
                }
            }
        }

        Ok(())
    }

    fn execute_test_definition(
        definition: &rez_next_package::PackageTestDefinition,
        install_path: &Path,
        environment: &std::collections::HashMap<String, String>,
    ) -> Result<std::process::Output, std::io::Error> {
        match definition.command.expanded_for_root(install_path) {
            TestCommand::String(command) => {
                Self::execute_test_command_with_environment(&command, install_path, environment)
            }
            TestCommand::List(arguments) if !arguments.is_empty() => {
                std::process::Command::new(&arguments[0])
                    .args(&arguments[1..])
                    .current_dir(install_path)
                    .env_clear()
                    .envs(environment)
                    .output()
            }
            TestCommand::List(_) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "package test command cannot be empty",
            )),
        }
    }

    fn execute_test_command_with_environment(
        cmd: &str,
        install_path: &Path,
        environment: &std::collections::HashMap<String, String>,
    ) -> Result<std::process::Output, std::io::Error> {
        rez_next_package::shell_command(cmd)
            .current_dir(install_path)
            .env_clear()
            .envs(environment)
            .output()
    }
}

/// Expand the supported release VCS tag placeholders for a package.
fn format_release_tag(package: &Package, tag_format: &str) -> String {
    let version = package
        .version
        .as_ref()
        .map(|value| value.as_str())
        .unwrap_or("unknown");
    let qualified_name = package.qualified_name();

    tag_format
        .replace("{name}", &package.name)
        .replace("{version}", version)
        .replace("{qualified_name}", &qualified_name)
}

fn truncate_changelog(changelog: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return changelog.to_string();
    }
    changelog.chars().take(max_chars).collect()
}

fn append_python_release_metadata(
    content: &str,
    package: &Package,
) -> Result<String, RezCoreError> {
    let string_literal = |value: Option<&str>| {
        value
            .map(serde_json::to_string)
            .transpose()
            .map(|value| value.unwrap_or_else(|| "None".to_string()))
            .map_err(|error| {
                RezCoreError::PackageParse(format!("Failed to serialize release metadata: {error}"))
            })
    };
    let release_message = string_literal(package.release_message.as_deref())?;
    let changelog = string_literal(package.changelog.as_deref())?;
    let revision = string_literal(package.revision.as_deref())?;
    let previous_version = string_literal(
        package
            .previous_version
            .as_ref()
            .map(|version| version.as_str()),
    )?;
    let previous_revision = string_literal(package.previous_revision.as_deref())?;
    let vcs = string_literal(package.vcs.as_deref())?;

    Ok(format!(
        "{content}\n\n# Release metadata written by rez-next\n\
timestamp = {}\n\
revision = {revision}\n\
changelog = {changelog}\n\
release_message = {release_message}\n\
previous_version = {previous_version}\n\
previous_revision = {previous_revision}\n\
vcs = {vcs}\n",
        package.timestamp.unwrap_or_default()
    ))
}

/// Resolve the environment required to build one package variant.
///
/// Release, CLI build, and Python bindings all use the same repository and
/// solver path through this function. An empty requirement set intentionally
/// returns `None`, allowing build systems that do not need a Rez context to
/// start without touching package repositories.
pub fn resolve_build_context(
    package: &Package,
    variant_requires: Option<&[String]>,
) -> Result<Option<rez_next_context::ResolvedContext>, RezCoreError> {
    let mut requirement_strings = Vec::new();
    for requirement in package
        .requires
        .iter()
        .chain(package.build_requires.iter())
        .chain(package.private_build_requires.iter())
        .chain(variant_requires.unwrap_or(&[]).iter())
    {
        if !requirement_strings.contains(requirement) {
            requirement_strings.push(requirement.clone());
        }
    }

    if requirement_strings.is_empty() {
        return Ok(None);
    }

    let mut repository_manager = RepositoryManager::new();
    let config = RezCoreConfig::load();
    for (index, search_path) in config.packages_path.iter().enumerate() {
        let path = PathBuf::from(expand_home(search_path));
        if path.exists() {
            let repository =
                FilesystemPackageRepository::with_name(path, format!("release_repo_{index}"));
            repository_manager.add_repository(Box::new(AsyncRepositoryAdapter::new(repository)));
        }
    }

    let requirements = requirement_strings
        .iter()
        .map(|requirement| {
            requirement.parse::<Requirement>().map_err(|error| {
                RezCoreError::RequirementParse(format!(
                    "Failed to parse build requirement '{}': {}",
                    requirement, error
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let context_requirements = requirement_strings
        .iter()
        .map(|requirement| {
            PackageRequirement::parse(requirement).map_err(|error| {
                RezCoreError::RequirementParse(format!(
                    "Failed to parse build context requirement '{}': {}",
                    requirement, error
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let runtime = tokio::runtime::Runtime::new().map_err(|error| {
        RezCoreError::BuildError(format!("Failed to create build context runtime: {error}"))
    })?;
    let mut resolver = DependencyResolver::new(
        Arc::new(repository_manager),
        SolverConfig {
            strict_mode: true,
            ..SolverConfig::default()
        },
    );
    let resolution = runtime.block_on(resolver.resolve(requirements))?;

    let mut context = rez_next_context::ResolvedContext::from_requirements(context_requirements);
    context.resolved_packages = resolution
        .resolved_packages
        .into_iter()
        .map(|info| info.try_materialized_package())
        .collect::<Result<Vec<_>, _>>()?;
    context.status = rez_next_context::ContextStatus::Resolved;

    let environment_manager = EnvironmentManager::new(ContextConfig {
        inherit_parent_env: false,
        ..Default::default()
    });
    context.environment_vars =
        runtime.block_on(environment_manager.generate_environment(&context.resolved_packages))?;

    Ok(Some(context))
}

/// Expand `~` in path to the user's home directory
fn expand_home(path: &str) -> String {
    if path.starts_with("~") {
        // Try to get home directory from environment variables
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        if !home.is_empty() {
            return path.replacen("~", &home, 1);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcs::{StubVCS, VCSMetadata};
    use rez_next_release_hook::{ReleaseHook, ReleaseHookError};
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn create_test_package(dir: &Path, name: &str, version: &str) -> PathBuf {
        let pkg_file = dir.join("package.py");
        let content = format!(
            r#"name = "{}"
version = "{}"
"#,
            name, version
        );
        let mut file = File::create(&pkg_file).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        pkg_file
    }

    /// The directory `Local` mode installs a package version into.
    fn install_dir_for(name: &str, version: &str) -> PathBuf {
        let config = RezCoreConfig::load();
        PathBuf::from(expand_home(&config.local_packages_path))
            .join(name)
            .join(version)
    }

    #[test]
    fn test_release_mode_from_str() {
        assert_eq!(ReleaseMode::from_str("release"), ReleaseMode::Release);
        assert_eq!(ReleaseMode::from_str("local"), ReleaseMode::Local);
        assert_eq!(ReleaseMode::from_str("dry-run"), ReleaseMode::DryRun);
        assert_eq!(ReleaseMode::from_str("dry_run"), ReleaseMode::DryRun);
        assert_eq!(ReleaseMode::from_str("unknown"), ReleaseMode::Release);
    }

    #[test]
    fn test_release_manager_new() {
        let manager = ReleaseManager::new(ReleaseMode::Release, false, false);
        assert!(manager.ensure_latest);
    }

    #[test]
    fn test_skip_repo_errors_only_suppresses_release_vcs_errors() {
        let mut manager = ReleaseManager::new(ReleaseMode::DryRun, true, true);
        manager.set_skip_repo_errors(true);
        let mut result = ReleaseResult::default();

        assert!(manager.skip_repo_error(
            "validate",
            &RezCoreError::ReleaseVCSError("repository unavailable".to_string()),
            &mut result
        ));
        assert!(!manager.skip_repo_error(
            "validate",
            &RezCoreError::BuildError("build unavailable".to_string()),
            &mut result
        ));
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("repository unavailable"));
    }

    #[test]
    fn test_release_manager_can_disable_latest_version_check() {
        let mut manager = ReleaseManager::new(ReleaseMode::Release, false, false);
        manager.set_ensure_latest(false);
        assert!(!manager.ensure_latest);
    }

    #[test]
    fn test_release_tag_format_expands_supported_placeholders() {
        let mut package = Package::new("test_pkg".to_string());
        package.version = Some(rez_next_version::Version::parse("1.0.0").unwrap());

        assert_eq!(
            format_release_tag(&package, "{qualified_name}"),
            "test_pkg-1.0.0"
        );
        assert_eq!(
            format_release_tag(&package, "release-{name}"),
            "release-test_pkg"
        );
        assert_eq!(format_release_tag(&package, "v{version}"), "v1.0.0");
        assert_eq!(
            format_release_tag(&package, "{name}/{version}/{qualified_name}"),
            "test_pkg/1.0.0/test_pkg-1.0.0"
        );
    }

    #[test]
    fn test_load_package_success() {
        let temp_dir = TempDir::new().unwrap();
        create_test_package(temp_dir.path(), "test_pkg", "1.0.0");

        let manager = ReleaseManager::new(ReleaseMode::DryRun, true, true);
        let mut result = ReleaseResult::default();

        let pkg = manager.load_package(temp_dir.path(), &mut result);
        assert!(pkg.is_ok());
        assert_eq!(result.package_name, "test_pkg");
        assert_eq!(result.version, "1.0.0");
    }

    #[test]
    fn test_run_tests_uses_declared_package_test_definitions() {
        let source_dir = TempDir::new().unwrap();
        let install_dir = TempDir::new().unwrap();
        let package_file = source_dir.path().join("package.py");
        std::fs::write(&package_file, "name = 'release_test'\nversion = '1.0.0'\n").unwrap();

        let mut package = Package::new("release_test".to_string());
        package.version = Some(rez_next_version::Version::parse("1.0.0").unwrap());
        package.filepath = Some(package_file.to_string_lossy().into_owned());
        package.tests.insert(
            "pre-release".to_string(),
            rez_next_package::PackageTestDefinition {
                command: TestCommand::List(vec![
                    "python".to_string(),
                    "-c".to_string(),
                    "import os,sys; sys.exit(0 if os.environ.get('RELEASE_TEST_ROOT') else 7)"
                        .to_string(),
                ]),
                requires: Vec::new(),
                run_on: vec!["pre_release".to_string()],
                on_variants: None,
            },
        );

        let manager = ReleaseManager::new(ReleaseMode::Local, true, false);
        let mut result = ReleaseResult::default();
        manager
            .run_tests(&package, install_dir.path(), &mut result)
            .unwrap();

        assert!(
            result.errors.is_empty(),
            "package test failed: {:?}",
            result.errors
        );
    }

    struct CountingReleaseHook {
        calls: std::sync::Arc<AtomicUsize>,
        cancel: bool,
        fail: bool,
    }

    impl ReleaseHook for CountingReleaseHook {
        fn name() -> String {
            "counting-release-hook".to_string()
        }

        fn new(_source_path: PathBuf) -> std::result::Result<Self, ReleaseHookError> {
            unreachable!("the test registry factory constructs this hook")
        }

        fn pre_release(
            &self,
            _user: &str,
            _install_path: &Path,
            _variants: Option<&[usize]>,
            _release_message: Option<&str>,
            _changelog: Option<&[String]>,
            _previous_version: Option<&str>,
            _previous_revision: Option<&str>,
        ) -> std::result::Result<(), ReleaseHookError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.cancel {
                Err(ReleaseHookError::Cancelled("test cancellation".to_string()))
            } else if self.fail {
                Err(ReleaseHookError::Error("test failure".to_string()))
            } else {
                Ok(())
            }
        }

        fn post_release(
            &self,
            _user: &str,
            _install_path: &Path,
            _variants: &[String],
            _release_message: Option<&str>,
            _changelog: Option<&[String]>,
            _previous_version: Option<&str>,
            _previous_revision: Option<&str>,
        ) -> std::result::Result<(), ReleaseHookError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(ReleaseHookError::Error("test failure".to_string()))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn test_release_hooks_run_for_pre_and_post_release() {
        let hook_name = "counting-release-hook-lifecycle";
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let factory_calls = calls.clone();
        rez_next_release_hook::get_registry().register(hook_name, move |_path| {
            Ok(Box::new(CountingReleaseHook {
                calls: factory_calls.clone(),
                cancel: false,
                fail: false,
            }) as Box<dyn ReleaseHook>)
        });

        let mut manager = ReleaseManager::new(ReleaseMode::Local, true, true);
        manager.set_release_hooks(Some(vec![hook_name.to_string()]));
        let source_dir = TempDir::new().unwrap();
        let install_dir = TempDir::new().unwrap();
        let package = Package::new("hooked-package".to_string());
        let mut result = ReleaseResult::default();

        assert!(manager.run_release_hooks(
            ReleaseHookEvent::PreRelease,
            source_dir.path(),
            &package,
            install_dir.path(),
            Some("release"),
            None,
            None,
            &mut result,
        ));
        assert!(manager.run_release_hooks(
            ReleaseHookEvent::PostRelease,
            source_dir.path(),
            &package,
            install_dir.path(),
            Some("release"),
            None,
            None,
            &mut result,
        ));

        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn test_release_hook_cancellation_becomes_release_error() {
        let hook_name = "counting-release-hook-cancel";
        rez_next_release_hook::get_registry().register(hook_name, |_path| {
            Ok(Box::new(CountingReleaseHook {
                calls: std::sync::Arc::new(AtomicUsize::new(0)),
                cancel: true,
                fail: false,
            }) as Box<dyn ReleaseHook>)
        });

        let mut manager = ReleaseManager::new(ReleaseMode::Local, true, true);
        manager.set_release_hooks(Some(vec![hook_name.to_string()]));
        let source_dir = TempDir::new().unwrap();
        let install_dir = TempDir::new().unwrap();
        let package = Package::new("cancelled-package".to_string());
        let mut result = ReleaseResult::default();

        assert!(!manager.run_release_hooks(
            ReleaseHookEvent::PreRelease,
            source_dir.path(),
            &package,
            install_dir.path(),
            None,
            None,
            None,
            &mut result,
        ));
        assert!(
            result
                .errors
                .iter()
                .any(|error| error.contains("cancelled release"))
        );
    }

    #[test]
    fn test_release_hook_failure_is_a_warning() {
        let hook_name = "counting-release-hook-error";
        rez_next_release_hook::get_registry().register(hook_name, |_path| {
            Ok(Box::new(CountingReleaseHook {
                calls: std::sync::Arc::new(AtomicUsize::new(0)),
                cancel: false,
                fail: true,
            }) as Box<dyn ReleaseHook>)
        });

        let mut manager = ReleaseManager::new(ReleaseMode::Local, true, true);
        manager.set_release_hooks(Some(vec![hook_name.to_string()]));
        let source_dir = TempDir::new().unwrap();
        let install_dir = TempDir::new().unwrap();
        let package = Package::new("hook-warning-package".to_string());
        let mut result = ReleaseResult::default();

        assert!(manager.run_release_hooks(
            ReleaseHookEvent::PreRelease,
            source_dir.path(),
            &package,
            install_dir.path(),
            None,
            None,
            None,
            &mut result,
        ));
        assert!(result.errors.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("test failure"))
        );
    }

    #[test]
    fn test_load_package_no_file() {
        let temp_dir = TempDir::new().unwrap();
        let manager = ReleaseManager::new(ReleaseMode::DryRun, true, true);
        let mut result = ReleaseResult::default();

        let pkg = manager.load_package(temp_dir.path(), &mut result);
        // load_package now returns Ok with default Package when file is missing
        assert!(
            pkg.is_ok(),
            "load_package should return Ok with default Package"
        );
        assert!(
            !result.errors.is_empty(),
            "should have errors when package file is missing"
        );
    }

    #[test]
    fn test_write_vcs_metadata_creates_file() {
        // Create temp dirs
        let source_dir = TempDir::new().unwrap();
        let install_dir = TempDir::new().unwrap();

        // Create package.py in source
        let pkg_file = source_dir.path().join("package.py");
        let content = r#"name = "test_pkg"
version = "1.0.0"
"#;
        std::fs::write(&pkg_file, content).unwrap();

        // Create StubVCS with metadata
        let metadata = VCSMetadata {
            vcs_type: "stub".to_string(),
            repository_url: Some("https://example.com/repo.git".to_string()),
            branch: Some("main".to_string()),
            commit_hash: "abc123".to_string(),
            ..Default::default()
        };
        let vcs = StubVCS::with_metadata(source_dir.path().to_path_buf(), metadata);

        // Create ReleaseManager (dry-run mode)
        let manager = ReleaseManager::new(ReleaseMode::DryRun, true, true);

        // Manually call write_vcs_metadata
        let mut result = ReleaseResult::default();
        let vcs: std::sync::Arc<dyn ReleaseVCS + Send + Sync> = std::sync::Arc::new(vcs);
        manager
            .write_vcs_metadata(install_dir.path(), &Some(vcs), &mut result)
            .unwrap();

        // Verify vcs_metadata.json was created
        let metadata_path = install_dir.path().join("vcs_metadata.json");
        assert!(
            metadata_path.exists(),
            "vcs_metadata.json should be created"
        );

        // Verify JSON content
        let json_content = std::fs::read_to_string(&metadata_path).unwrap();
        assert!(json_content.contains("stub"), "Should contain vcs_type");
        assert!(
            json_content.contains("abc123"),
            "Should contain commit_hash"
        );
        assert!(json_content.contains("main"), "Should contain branch");

        // Verify result has vcs_metadata
        assert!(result.vcs_metadata.is_some());
        let vcs_meta = result.vcs_metadata.unwrap();
        assert_eq!(vcs_meta.vcs_type, "stub");
        assert_eq!(vcs_meta.commit_hash, "abc123");
    }

    #[test]
    fn test_write_vcs_metadata_no_vcs() {
        let install_dir = TempDir::new().unwrap();

        // No VCS
        let vcs: Option<std::sync::Arc<dyn ReleaseVCS + Send + Sync>> = None;

        // Create ReleaseManager
        let manager = ReleaseManager::new(ReleaseMode::DryRun, true, true);

        // Call write_vcs_metadata with no VCS
        let mut result = ReleaseResult::default();
        manager
            .write_vcs_metadata(install_dir.path(), &vcs, &mut result)
            .unwrap();

        // Verify vcs_metadata.json was NOT created
        let metadata_path = install_dir.path().join("vcs_metadata.json");
        assert!(
            !metadata_path.exists(),
            "vcs_metadata.json should NOT be created when no VCS"
        );

        // Verify warning was added
        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].contains("No VCS detected"));
    }

    /// A VCS whose tags can be forced to exist, so `create_vcs_tag` can be
    /// exercised for the already-released case.
    struct ExistingTagVCS {
        tag_exists: bool,
        created: std::sync::Mutex<Vec<String>>,
    }

    impl crate::vcs::ReleaseVCS for ExistingTagVCS {
        fn get_type_name(&self) -> &str {
            "existing-tag-stub"
        }
        fn get_repo_root(&self) -> Result<PathBuf, RezCoreError> {
            Ok(PathBuf::from("."))
        }
        fn is_clean(&self) -> Result<bool, RezCoreError> {
            Ok(true)
        }
        fn get_current_branch(&self) -> Result<String, RezCoreError> {
            Ok("main".to_string())
        }
        fn get_latest_commit(&self) -> Result<String, RezCoreError> {
            Ok("stub-commit".to_string())
        }
        fn tag_exists(&self, _tag: &str) -> Result<bool, RezCoreError> {
            Ok(self.tag_exists)
        }
        fn create_tag(&self, tag: &str, _message: &str) -> Result<(), RezCoreError> {
            self.created.lock().unwrap().push(tag.to_string());
            Ok(())
        }
        fn get_changelog(
            &self,
            _previous_revision: Option<&str>,
            _max_revisions: Option<usize>,
        ) -> Result<String, RezCoreError> {
            Ok(String::new())
        }
        fn get_metadata(&self) -> Result<crate::vcs::VCSMetadata, RezCoreError> {
            Ok(crate::vcs::VCSMetadata::default())
        }
    }

    fn manager_with_tag_policy(
        ignore_existing_tag: Option<bool>,
    ) -> (ReleaseManager, std::sync::Arc<ExistingTagVCS>) {
        let mut manager = ReleaseManager::new(ReleaseMode::Release, true, true);
        manager.set_ignore_existing_tag(ignore_existing_tag);
        (
            manager,
            std::sync::Arc::new(ExistingTagVCS {
                tag_exists: true,
                created: std::sync::Mutex::new(Vec::new()),
            }),
        )
    }

    /// Drive `ReleaseManager::release()` end to end with a stub VCS, so the tag
    /// policy and its position in the step order are both exercised.
    ///
    /// `build_and_test` controls whether the build and test steps run; turning
    /// it off keeps the run hermetic (no `python` lookup, no shelling out).
    ///
    /// `refuse_existing` makes the stub fail `create_tag` for an existing tag,
    /// the way the real providers do. It is on by default, including when
    /// `tag_exists` is false, where it has no effect.
    fn run_release_with_policy(
        source_dir: &Path,
        name: &str,
        version: &str,
        ignore_existing_tag: Option<bool>,
        tag_exists: bool,
        build_and_test: bool,
    ) -> (ReleaseResult, std::sync::Arc<CountingTagVCS>) {
        init_git_repo(source_dir);
        create_test_package(source_dir, name, version);

        let mut manager = ReleaseManager::new(ReleaseMode::Local, !build_and_test, !build_and_test);
        manager.set_skip_vcs_validation(true);
        manager.set_ignore_existing_tag(ignore_existing_tag);

        let vcs = std::sync::Arc::new(CountingTagVCS {
            tag_exists,
            refuse_existing: true,
            created: std::sync::Mutex::new(Vec::new()),
        });
        let vcs_for_release: std::sync::Arc<dyn crate::vcs::ReleaseVCS + Send + Sync> = vcs.clone();

        let result = manager
            .release_with_vcs(source_dir, None, Some(vcs_for_release))
            .expect("release() must not propagate an error");

        (result, vcs)
    }

    /// Create a directory that `detect_vcs` recognises as a git repository.
    fn init_git_repo(dir: &Path) {
        fs::create_dir_all(dir.join(".git")).expect("create .git dir");
    }

    /// A VCS stub that records every tag it is asked to create.
    ///
    /// With `refuse_existing` set it also fails `create_tag` for a tag that
    /// already exists, mirroring the real providers, which create tags with
    /// `force = false` and do not pre-check for existence. Tests that care
    /// about a release surviving an existing tag should turn it on: a stub
    /// that happily re-tags would hide the failure.
    struct CountingTagVCS {
        tag_exists: bool,
        refuse_existing: bool,
        created: std::sync::Mutex<Vec<String>>,
    }

    impl CountingTagVCS {
        fn created_tags(&self) -> Vec<String> {
            self.created.lock().unwrap().clone()
        }
    }

    impl crate::vcs::ReleaseVCS for CountingTagVCS {
        fn get_type_name(&self) -> &str {
            "counting-stub"
        }
        fn get_repo_root(&self) -> Result<PathBuf, RezCoreError> {
            Ok(PathBuf::from("."))
        }
        fn is_clean(&self) -> Result<bool, RezCoreError> {
            Ok(true)
        }
        fn get_current_branch(&self) -> Result<String, RezCoreError> {
            Ok("main".to_string())
        }
        fn get_latest_commit(&self) -> Result<String, RezCoreError> {
            Ok("stub-commit".to_string())
        }
        fn tag_exists(&self, _tag: &str) -> Result<bool, RezCoreError> {
            Ok(self.tag_exists)
        }
        fn create_tag(&self, tag: &str, _message: &str) -> Result<(), RezCoreError> {
            if self.refuse_existing && self.tag_exists {
                return Err(RezCoreError::ReleaseVCSError(format!(
                    "tag '{}' already exists",
                    tag
                )));
            }
            self.created.lock().unwrap().push(tag.to_string());
            Ok(())
        }
        fn get_changelog(
            &self,
            _previous_revision: Option<&str>,
            _max_revisions: Option<usize>,
        ) -> Result<String, RezCoreError> {
            Ok(String::new())
        }
        fn get_metadata(&self) -> Result<crate::vcs::VCSMetadata, RezCoreError> {
            Ok(crate::vcs::VCSMetadata::default())
        }
    }

    /// `release()` must reject an existing tag *before* the build step, so an
    /// already-installed copy of the package is left byte-for-byte untouched.
    ///
    /// Regression guard: adding the tag policy without moving the check ahead
    /// of the build returned the right exit code but still overwrote the
    /// installed package.
    #[test]
    fn test_release_rejects_existing_tag_without_touching_installed_copy() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("src");
        fs::create_dir_all(&source).unwrap();

        let installed = install_dir_for("released_pkg", "1.0.0");
        fs::create_dir_all(&installed).unwrap();
        let installed_copy = installed.join("package.py");
        let sentinel = b"# the already-released package.py\n";
        fs::write(&installed_copy, sentinel).unwrap();

        let (result, vcs) =
            run_release_with_policy(&source, "released_pkg", "1.0.0", Some(false), true, false);

        assert!(
            result.errors.iter().any(|e| e.contains("already exists")),
            "strict mode must fail the release, got errors {:?}",
            result.errors
        );
        assert!(!result.success, "success must be false when rejected");
        assert_eq!(
            fs::read(&installed_copy).unwrap(),
            sentinel.to_vec(),
            "the installed package.py must not be overwritten"
        );
        assert!(
            vcs.created_tags().is_empty(),
            "a rejected release must not create a tag, got {:?}",
            vcs.created_tags()
        );
    }

    /// A release whose build fails must not leave a tag behind: the invariant
    /// is **tag exists ⟺ release succeeded**, so a dangling tag would wedge
    /// the version on every retry.
    ///
    /// The failure is induced inside the build step — after the tag policy
    /// check has passed, which is exactly the window in which a tag created up
    /// front survives a release that never ships. A variant whose hashed
    /// subdirectory cannot be created makes `build_package` record an error and
    /// continue, so the release ends with `success == false` instead of
    /// aborting early.
    #[test]
    fn test_release_does_not_create_tag_when_build_fails() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("src");
        fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);

        fs::write(
            source.join("package.py"),
            "name = \"wedged_pkg\"\nversion = \"1.0.0\"\nvariants = [[\"python-3.9\"]]\n",
        )
        .unwrap();

        let install_dir = install_dir_for("wedged_pkg", "1.0.0");
        fs::create_dir_all(&install_dir).unwrap();

        // Occupy the canonical requirement subpath so only the variant fails.
        fs::write(install_dir.join("python-3.9"), "not a directory").unwrap();

        let mut manager = ReleaseManager::new(ReleaseMode::Local, false, true);
        manager.set_skip_vcs_validation(true);
        manager.set_ignore_existing_tag(Some(false));

        let vcs = std::sync::Arc::new(CountingTagVCS {
            tag_exists: false,
            refuse_existing: true,
            created: std::sync::Mutex::new(Vec::new()),
        });
        let vcs_for_release: std::sync::Arc<dyn crate::vcs::ReleaseVCS + Send + Sync> = vcs.clone();

        let result = manager
            .release_with_vcs(&source, None, Some(vcs_for_release))
            .expect("release() must not propagate an error");

        assert!(
            result.errors.iter().any(|e| {
                e.contains("Failed to create variant directory")
                    || e.contains("Failed to resolve build context")
            }),
            "a failed build or build-context resolution must fail the release, got {:?}",
            result.errors
        );
        assert!(
            !result.success,
            "success must be false after a failed build"
        );
        assert!(
            vcs.created_tags().is_empty(),
            "a failed build must not create a tag, got {:?}",
            vcs.created_tags()
        );
    }

    /// The complement of the two tests above: a successful release *does* tag,
    /// and tags exactly once.
    #[test]
    fn test_release_creates_tag_on_success() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("src");
        fs::create_dir_all(&source).unwrap();

        let installed = install_dir_for("fresh_pkg", "2.0.0");
        fs::create_dir_all(&installed).unwrap();

        let (result, vcs) =
            run_release_with_policy(&source, "fresh_pkg", "2.0.0", Some(false), false, false);

        assert!(
            result.errors.is_empty(),
            "a clean release must succeed, got {:?}",
            result.errors
        );
        assert!(result.success, "success must be true");
        assert_eq!(
            vcs.created_tags(),
            vec!["fresh_pkg-2.0.0".to_string()],
            "a successful release must create its tag exactly once"
        );
    }

    #[test]
    fn test_release_installs_release_provenance() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("src");
        fs::create_dir_all(&source).unwrap();
        init_git_repo(&source);
        create_test_package(&source, "provenance_pkg", "3.0.0");
        fs::create_dir_all(install_dir_for("provenance_pkg", "3.0.0")).unwrap();

        let mut manager = ReleaseManager::new(ReleaseMode::Local, true, true);
        manager.set_skip_vcs_validation(true);
        manager.set_ignore_existing_tag(Some(false));
        let vcs = std::sync::Arc::new(CountingTagVCS {
            tag_exists: false,
            refuse_existing: true,
            created: std::sync::Mutex::new(Vec::new()),
        });
        let vcs_for_release: std::sync::Arc<dyn crate::vcs::ReleaseVCS + Send + Sync> = vcs;

        let result = manager
            .release_with_vcs(&source, Some("publish provenance"), Some(vcs_for_release))
            .expect("release() must not propagate an error");

        assert!(result.success, "release failed: {:?}", result.errors);
        let installed = PackageSerializer::load_from_file(
            &install_dir_for("provenance_pkg", "3.0.0").join("package.py"),
        )
        .expect("installed package definition should remain parseable");
        assert!(installed.timestamp.is_some());
        assert_eq!(installed.revision.as_deref(), Some("stub-commit"));
        assert_eq!(installed.changelog.as_deref(), Some(""));
        assert_eq!(
            installed.release_message.as_deref(),
            Some("publish provenance")
        );
        assert_eq!(installed.vcs.as_deref(), Some("counting-stub"));
    }

    #[test]
    fn test_truncate_changelog_zero_keeps_full_content() {
        assert_eq!(truncate_changelog("first\nsecond", 0), "first\nsecond");
        assert_eq!(truncate_changelog("first\nsecond", 5), "first");
    }

    /// `--ignore-existing-tag` over an existing tag: the package is installed
    /// and the release still reports success, without touching the tag.
    ///
    /// The stub refuses to re-create an existing tag, exactly like the real
    /// providers (`force = false`, no existence pre-check). So if step 9 tried
    /// to tag anyway, the release would end up installed but reported as
    /// failed — the regression this pins down.
    #[test]
    fn test_release_ignore_existing_tag_succeeds_without_retagging() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("src");
        fs::create_dir_all(&source).unwrap();

        let installed = install_dir_for("reissue_pkg", "1.0.0");
        fs::create_dir_all(&installed).unwrap();

        let (result, vcs) =
            run_release_with_policy(&source, "reissue_pkg", "1.0.0", Some(true), true, false);

        assert!(
            result.errors.is_empty(),
            "an explicitly ignored existing tag must not fail the release, got {:?}",
            result.errors
        );
        assert!(result.success, "success must be true");
        assert!(
            vcs.created_tags().is_empty(),
            "an existing tag must not be re-created, got {:?}",
            vcs.created_tags()
        );
        assert!(
            result.warnings.iter().any(|w| w.contains("already exists")),
            "the existing-tag warning must be kept, got {:?}",
            result.warnings
        );
        assert!(
            installed.join("package.py").exists(),
            "the package must still be installed"
        );
    }

    /// The legacy default (`None`) over an existing tag: warn, install, and
    /// still report success — the behaviour that predates this work and that
    /// the `Legacy: warn and continue` comment claims to preserve.
    #[test]
    fn test_release_legacy_policy_succeeds_without_retagging() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("src");
        fs::create_dir_all(&source).unwrap();

        let installed = install_dir_for("legacy_pkg", "1.0.0");
        fs::create_dir_all(&installed).unwrap();

        let (result, vcs) =
            run_release_with_policy(&source, "legacy_pkg", "1.0.0", None, true, false);

        assert!(
            result.errors.is_empty(),
            "the legacy default must not start failing, got {:?}",
            result.errors
        );
        assert!(result.success, "success must be true");
        assert!(
            vcs.created_tags().is_empty(),
            "an existing tag must not be re-created, got {:?}",
            vcs.created_tags()
        );
        assert!(
            result.warnings.iter().any(|w| w.contains("already exists")),
            "the legacy default still warns, got {:?}",
            result.warnings
        );
        assert!(
            installed.join("package.py").exists(),
            "the package must still be installed"
        );
    }

    #[test]
    fn test_check_existing_tag_strict_rejects() {
        let (manager, vcs) = manager_with_tag_policy(Some(false));
        let mut result = ReleaseResult::default();
        let mut package = rez_next_package::Package::new("tool".to_string());
        package.version = Some(rez_next_version::Version::new(Some("1.0.0")).unwrap());

        manager
            .check_existing_tag(&*vcs, &package, &mut result)
            .unwrap();

        assert!(
            result.errors.iter().any(|e| e.contains("already exists")),
            "strict mode must record an error, got {:?}",
            result.errors
        );
        assert!(
            result
                .warnings
                .iter()
                .all(|w| !w.contains("already exists")),
            "strict mode must not downgrade the rejection to a warning"
        );
        assert!(
            vcs.created.lock().unwrap().is_empty(),
            "the policy check must never create a tag"
        );
    }

    #[test]
    fn test_check_existing_tag_ignore_allows() {
        let (manager, vcs) = manager_with_tag_policy(Some(true));
        let mut result = ReleaseResult::default();
        let mut package = rez_next_package::Package::new("tool".to_string());
        package.version = Some(rez_next_version::Version::new(Some("1.0.0")).unwrap());

        manager
            .check_existing_tag(&*vcs, &package, &mut result)
            .unwrap();

        assert!(
            result.errors.is_empty(),
            "explicit opt-in must proceed, got {:?}",
            result.errors
        );
        assert!(result.warnings.iter().any(|w| w.contains("already exists")));
    }

    #[test]
    fn test_check_existing_tag_default_warns_and_continues() {
        let (manager, vcs) = manager_with_tag_policy(None);
        let mut result = ReleaseResult::default();
        let mut package = rez_next_package::Package::new("tool".to_string());
        package.version = Some(rez_next_version::Version::new(Some("1.0.0")).unwrap());

        manager
            .check_existing_tag(&*vcs, &package, &mut result)
            .unwrap();

        assert!(
            result.errors.is_empty(),
            "the legacy default must not start failing, got {:?}",
            result.errors
        );
        assert!(
            result.warnings.iter().any(|w| w.contains("already exists")),
            "the legacy default still warns"
        );
    }

    /// `create_vcs_tag` is now purely the tag-creation step; it must create the
    /// tag and report it, leaving policy to `check_existing_tag`.
    #[test]
    fn test_create_vcs_tag_creates_tag() {
        let mut manager = ReleaseManager::new(ReleaseMode::Release, true, true);
        manager.set_ignore_existing_tag(None);
        let vcs = std::sync::Arc::new(ExistingTagVCS {
            tag_exists: false,
            created: std::sync::Mutex::new(Vec::new()),
        });
        let mut result = ReleaseResult::default();
        let mut package = rez_next_package::Package::new("tool".to_string());
        package.version = Some(rez_next_version::Version::new(Some("1.0.0")).unwrap());

        manager
            .create_vcs_tag(&*vcs, &package, None, &mut result)
            .unwrap();

        assert!(
            result.errors.is_empty(),
            "tag creation must succeed, got {:?}",
            result.errors
        );
        assert_eq!(
            vcs.created.lock().unwrap().as_slice(),
            ["tool-1.0.0".to_string()],
            "the release tag must be created once"
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("Created VCS tag"))
        );
    }
}
