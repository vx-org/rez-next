//! Version Control System integration for release management
//!
//! This module provides the `ReleaseVCS` trait and implementations for
//! different version control systems (Git, Mercurial, SVN, etc.)
//!
//! The implementations are split into separate files:
//! - `git.rs` — `GitVCS` implementation (requires feature "git")
//! - `hg.rs` — `MercurialVCS` implementation
//! - `svn.rs` — `SvnVCS` implementation

#[cfg(feature = "git")]
mod git;

mod hg;
mod svn;

// Re-export VCS implementations for Python bindings
#[cfg(feature = "git")]
pub use git::GitVCS;
pub use hg::MercurialVCS;
pub use svn::SvnVCS;

use rez_next_common::RezCoreError;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The result of VCS root discovery.
///
/// `levels_up` preserves the proximity information exposed by Rez's
/// `find_vcs_root()` contract. A caller can use it to prefer the nearest
/// repository when several VCS types are available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VCSRoot {
    /// Absolute path to the repository root.
    pub path: PathBuf,
    /// Number of parent directories traversed from the input path.
    pub levels_up: usize,
}

/// Find a repository root by checking the input path and, optionally, parents.
pub fn find_vcs_root(
    path: &Path,
    search_parents: bool,
    is_valid_root: impl Fn(&Path) -> bool,
) -> Option<(PathBuf, usize)> {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut current = std::fs::canonicalize(&absolute_path).unwrap_or(absolute_path);
    if current.is_file() {
        current = current.parent()?.to_path_buf();
    }

    for levels_up in 0.. {
        if is_valid_root(&current) {
            return Some((current, levels_up));
        }
        if !search_parents {
            return None;
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }

    None
}

/// Find an executable in `PATH` for a release VCS implementation.
pub fn find_vcs_executable(name: &str) -> Result<PathBuf, RezCoreError> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }

        #[cfg(windows)]
        {
            let candidate = directory.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(RezCoreError::ReleaseVCSError(format!(
        "VCS executable not found: {name}"
    )))
}

// ============================================================================
// VCS Revision
// ============================================================================

/// A type-erased VCS revision that can be serialized/deserialized.
///
/// This aligns with Rez's `get_current_revision()` which can return
/// "any type (str, dict etc.)". We use a structured representation
/// that preserves the revision data in a JSON-compatible format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VCSRevision {
    /// VCS type (git, hg, svn, etc.)
    pub revision_type: String,
    /// The revision identifier (commit hash, changeset hash, revision number, etc.)
    pub revision_id: String,
    /// Full revision data (can be a string, dict, etc.)
    pub data: JsonValue,
    /// Additional metadata (branch, tags, etc.)
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl VCSRevision {
    /// Create a new VCSRevision
    pub fn new(revision_type: &str, revision_id: &str) -> Self {
        Self {
            revision_type: revision_type.to_string(),
            revision_id: revision_id.to_string(),
            data: JsonValue::String(revision_id.to_string()),
            metadata: HashMap::new(),
        }
    }

    /// Create a VCSRevision with custom data
    pub fn with_data(revision_type: &str, revision_id: &str, data: JsonValue) -> Self {
        Self {
            revision_type: revision_type.to_string(),
            revision_id: revision_id.to_string(),
            data,
            metadata: HashMap::new(),
        }
    }
}

/// Trait for VCS integration in release workflow
///
/// This trait defines the interface for interacting with version control
/// systems during the package release process.
/// Compatible with original rez ReleaseVCS interface.
///
/// # Implementing a custom VCS
///
/// External crates implement this trait and pass an
/// `Arc<dyn ReleaseVCS + Send + Sync>` to
/// [`ReleaseManager::release_with_vcs`](crate::ReleaseManager::release_with_vcs)
/// to drive a release with their own version-control backend. Only
/// [`ReleaseVCS::get_type_name`], [`ReleaseVCS::get_repo_root`],
/// [`ReleaseVCS::is_clean`], [`ReleaseVCS::get_current_branch`],
/// [`ReleaseVCS::get_latest_commit`], [`ReleaseVCS::tag_exists`],
/// [`ReleaseVCS::create_tag`], [`ReleaseVCS::get_changelog`] and
/// [`ReleaseVCS::get_metadata`] are required. Root discovery and the
/// upstream spelling of repository validation are available as additive
/// hooks, so a custom provider can be ported without reimplementing path
/// walking or executable lookup.
///
/// Three behaviours the release flow depends on:
///
/// - [`ReleaseVCS::create_tag`] must not move an existing tag. The release flow
///   never asks for that (it skips tagging when the tag already exists), and
///   moving one would detach a released version from its provenance.
/// - Most failures are non-fatal and downgraded to a warning:
///   [`ReleaseVCS::get_metadata`], [`ReleaseVCS::get_changelog`] and
///   [`ReleaseVCS::tag_exists`] may return `Err` without failing the release.
///   [`ReleaseVCS::create_tag`] is the exception — its `Err` is recorded as a
///   release error and makes the release unsuccessful.
/// - [`ReleaseVCS::validate_repo_state`] failing aborts the release, but only
///   when VCS validation is enabled. Dedicated [`RezCoreError::ReleaseVCSError`]
///   values can be downgraded with
///   [`ReleaseManager::set_skip_repo_errors`](crate::ReleaseManager::set_skip_repo_errors).
///
/// The trait is already blanket-implemented for `Box<dyn ReleaseVCS + Send +
/// Sync>` and `Arc<dyn ReleaseVCS + Send + Sync>`, so a trait object can stand
/// in for an owned implementation.
pub trait ReleaseVCS: Send + Sync {
    /// Return whether `path` is a root for this VCS.
    ///
    /// This is an associated hook because root discovery happens before an
    /// implementation instance exists. The default keeps older providers
    /// source-compatible and reports no root until they opt in.
    fn is_valid_root(_path: &Path) -> bool
    where
        Self: Sized,
    {
        false
    }

    /// Return whether parent directories should be searched for a root.
    fn search_parents_for_root() -> bool
    where
        Self: Sized,
    {
        true
    }

    /// Find this VCS's nearest root and preserve its parent distance.
    fn find_vcs_root(path: &Path) -> Option<(PathBuf, usize)>
    where
        Self: Sized,
    {
        find_vcs_root(path, Self::search_parents_for_root(), Self::is_valid_root)
    }

    /// Find a VCS executable by name.
    fn find_executable(name: &str) -> Result<PathBuf, RezCoreError>
    where
        Self: Sized,
    {
        find_vcs_executable(name)
    }

    /// Get the VCS type name
    fn get_type_name(&self) -> &str;

    /// Get the repository root path
    fn get_repo_root(&self) -> Result<PathBuf, RezCoreError>;

    /// Get the repository root together with the discovery distance.
    ///
    /// Instances created with an explicit root have a distance of zero. A
    /// factory that discovers a root should retain the tuple returned by
    /// [`ReleaseVCS::find_vcs_root`] when it needs to compare providers.
    fn get_repo_root_info(&self) -> Result<VCSRoot, RezCoreError> {
        Ok(VCSRoot {
            path: self.get_repo_root()?,
            levels_up: 0,
        })
    }

    /// Check if the repository is clean (no uncommitted changes)
    fn is_clean(&self) -> Result<bool, RezCoreError>;

    /// Get the current branch name
    fn get_current_branch(&self) -> Result<String, RezCoreError>;

    /// Get the latest commit hash
    fn get_latest_commit(&self) -> Result<String, RezCoreError>;

    /// Check if a tag exists
    fn tag_exists(&self, tag: &str) -> Result<bool, RezCoreError>;

    /// Create a new tag using the Rust-friendly method name.
    ///
    /// The default delegates to [`ReleaseVCS::create_release_tag`], which
    /// allows an upstream-style provider to implement its original method
    /// name. Existing Rust providers can continue implementing `create_tag`.
    fn create_tag(&self, tag: &str, message: &str) -> Result<(), RezCoreError> {
        self.create_release_tag(tag, Some(message))
    }

    /// Create a release tag using Rez's upstream method name.
    fn create_release_tag(&self, _tag: &str, _message: Option<&str>) -> Result<(), RezCoreError> {
        Err(RezCoreError::ReleaseVCSError(
            "create_release_tag() not implemented for this VCS".to_string(),
        ))
    }

    /// Get changelog since a previous revision, optionally capped.
    fn get_changelog(
        &self,
        previous_revision: Option<&str>,
        max_revisions: Option<usize>,
    ) -> Result<String, RezCoreError>;

    /// Get VCS metadata for release
    fn get_metadata(&self) -> Result<VCSMetadata, RezCoreError>;

    /// Validate repository state for release using the upstream method name.
    ///
    /// Older Rust callers can use [`ReleaseVCS::validate_repo_state`], which
    /// delegates here.
    fn validate_repostate(&self) -> Result<(), RezCoreError> {
        if !self.is_clean()? {
            return Err(RezCoreError::ReleaseVCSError(
                "Repository is not clean".to_string(),
            ));
        }
        Ok(())
    }

    /// Validate repository state using the Rust-friendly spelling.
    fn validate_repo_state(&self) -> Result<(), RezCoreError> {
        self.validate_repostate()
    }

    /// Check if the current branch is a releasable branch
    ///
    /// For Git, this might check if we're on `main`, `master`, or a release branch.
    /// For Mercurial, this might check if we're on `default` or a named branch.
    /// Returns `None` if the concept doesn't apply to this VCS.
    fn is_releasable_branch(&self) -> Result<Option<bool>, RezCoreError> {
        // Default: not applicable (return None)
        Ok(None)
    }

    /// Get the current revision object.
    ///
    /// This aligns with Rez's `ReleaseVCS.get_current_revision()` which can return
    /// "any type (str, dict etc.)". We return a `VCSRevision` struct that
    /// encapsulates the revision data in a type-safe way.
    ///
    /// The returned `VCSRevision` contains:
    /// - `revision_type`: VCS type (git, hg, svn, etc.)
    /// - `revision_id`: The revision identifier (commit hash, changset hash, etc.)
    /// - `data`: Full revision data (JSON-compatible for flexibility)
    /// - `metadata`: Additional metadata (branch, tags, etc.)
    fn get_current_revision(&self) -> Result<VCSRevision, RezCoreError> {
        // Default: return the latest commit hash as the revision
        let commit_hash = self.get_latest_commit()?;
        Ok(VCSRevision::new(self.get_type_name(), &commit_hash))
    }

    /// Export the repository at the given revision to the given path.
    ///
    /// Export the repository through an instance backend.
    ///
    /// The additive [`ReleaseVCS::export_revision`] hook mirrors Rez's
    /// classmethod when a provider can export from the revision alone.
    ///
    /// # Arguments
    ///
    /// * `revision` - The revision to export (as `VCSRevision`)
    /// * `path` - The path to export to (must not exist, parent must exist)
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or `RezCoreError` on failure.
    fn export(&self, _revision: &VCSRevision, _path: &Path) -> Result<(), RezCoreError> {
        Err(RezCoreError::ReleaseVCSError(
            "export() not implemented for this VCS".to_string(),
        ))
    }

    /// Export a revision through a class-level provider hook.
    ///
    /// Rust release flows use [`ReleaseVCS::export`] because a trait object
    /// carries the repository instance. This associated hook mirrors Rez's
    /// classmethod for providers whose revision object contains all export
    /// information.
    fn export_revision(_revision: &VCSRevision, _path: &Path) -> Result<(), RezCoreError>
    where
        Self: Sized,
    {
        Err(RezCoreError::ReleaseVCSError(
            "export_revision() not implemented for this VCS".to_string(),
        ))
    }
}

/// VCS metadata for release
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VCSMetadata {
    /// VCS type (git, hg, svn, etc.)
    pub vcs_type: String,
    /// Repository URL (fetch URL)
    pub repository_url: Option<String>,
    /// Current branch name
    pub branch: Option<String>,
    /// Tracking branch name (e.g., "origin/main")
    pub tracking_branch: Option<String>,
    /// Fetch URL (where the repo is fetched from)
    pub fetch_url: Option<String>,
    /// Push URL (where the repo is pushed to)
    pub push_url: Option<String>,
    /// Latest commit hash
    pub commit_hash: String,
    /// Commit message
    pub commit_message: Option<String>,
    /// Author name
    pub author_name: Option<String>,
    /// Author email
    pub author_email: Option<String>,
    /// Timestamp of the commit
    pub timestamp: Option<i64>,
    /// Additional metadata
    #[serde(default)]
    pub extra: HashMap<String, String>,
}

/// Stub VCS implementation for explicit tests
///
/// This is a no-op implementation that returns dummy data.
/// Production VCS detection never selects this implementation.
#[derive(Debug)]
pub struct StubVCS {
    /// Repository root path
    repo_root: PathBuf,
    /// Optional metadata override for testing
    metadata: Option<VCSMetadata>,
}

impl StubVCS {
    /// Create a new StubVCS
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            repo_root,
            metadata: None,
        }
    }

    /// Create a new StubVCS with custom metadata
    pub fn with_metadata(repo_root: PathBuf, metadata: VCSMetadata) -> Self {
        Self {
            repo_root,
            metadata: Some(metadata),
        }
    }
}

impl ReleaseVCS for StubVCS {
    fn get_type_name(&self) -> &str {
        "stub"
    }

    fn get_repo_root(&self) -> Result<PathBuf, RezCoreError> {
        Ok(self.repo_root.clone())
    }

    fn is_clean(&self) -> Result<bool, RezCoreError> {
        Ok(true) // Stub is always clean
    }

    fn get_current_branch(&self) -> Result<String, RezCoreError> {
        Ok("main".to_string())
    }

    fn get_latest_commit(&self) -> Result<String, RezCoreError> {
        Ok("stub-commit-hash".to_string())
    }

    fn tag_exists(&self, _tag: &str) -> Result<bool, RezCoreError> {
        Ok(false) // Stub never has tags
    }

    fn create_tag(&self, _tag: &str, _message: &str) -> Result<(), RezCoreError> {
        Ok(()) // Stub always succeeds
    }

    fn create_release_tag(&self, tag: &str, message: Option<&str>) -> Result<(), RezCoreError> {
        self.create_tag(tag, message.unwrap_or_default())
    }

    fn get_changelog(
        &self,
        _previous_revision: Option<&str>,
        _max_revisions: Option<usize>,
    ) -> Result<String, RezCoreError> {
        Ok("Stub changelog".to_string())
    }

    fn get_metadata(&self) -> Result<VCSMetadata, RezCoreError> {
        if let Some(ref metadata) = self.metadata {
            return Ok(metadata.clone());
        }

        Ok(VCSMetadata {
            vcs_type: "stub".to_string(),
            commit_hash: "stub-commit-hash".to_string(),
            branch: Some("main".to_string()),
            ..Default::default()
        })
    }
}

/// Implement ReleaseVCS for Box<dyn ReleaseVCS + Send + Sync>
///
/// This allows using Box<dyn ReleaseVCS + Send + Sync> as a ReleaseVCS trait object.
impl ReleaseVCS for Box<dyn ReleaseVCS + Send + Sync> {
    fn get_type_name(&self) -> &str {
        (**self).get_type_name()
    }

    fn get_repo_root(&self) -> Result<PathBuf, RezCoreError> {
        (**self).get_repo_root()
    }

    fn is_clean(&self) -> Result<bool, RezCoreError> {
        (**self).is_clean()
    }

    fn get_current_branch(&self) -> Result<String, RezCoreError> {
        (**self).get_current_branch()
    }

    fn get_latest_commit(&self) -> Result<String, RezCoreError> {
        (**self).get_latest_commit()
    }

    fn tag_exists(&self, tag: &str) -> Result<bool, RezCoreError> {
        (**self).tag_exists(tag)
    }

    fn create_tag(&self, tag: &str, message: &str) -> Result<(), RezCoreError> {
        (**self).create_tag(tag, message)
    }

    fn create_release_tag(&self, tag: &str, message: Option<&str>) -> Result<(), RezCoreError> {
        (**self).create_release_tag(tag, message)
    }

    fn get_changelog(
        &self,
        previous_revision: Option<&str>,
        max_revisions: Option<usize>,
    ) -> Result<String, RezCoreError> {
        (**self).get_changelog(previous_revision, max_revisions)
    }

    fn get_metadata(&self) -> Result<VCSMetadata, RezCoreError> {
        (**self).get_metadata()
    }

    fn validate_repo_state(&self) -> Result<(), RezCoreError> {
        (**self).validate_repo_state()
    }

    fn is_releasable_branch(&self) -> Result<Option<bool>, RezCoreError> {
        (**self).is_releasable_branch()
    }
}

/// Implement ReleaseVCS for Arc<dyn ReleaseVCS + Send + Sync>
///
/// Lets a shared trait object stand in for an owned one, so callers that need
/// to observe the same VCS instance the release flow uses can hold an `Arc` to
/// it instead of boxing a fresh one.
impl ReleaseVCS for std::sync::Arc<dyn ReleaseVCS + Send + Sync> {
    fn get_type_name(&self) -> &str {
        (**self).get_type_name()
    }

    fn get_repo_root(&self) -> Result<PathBuf, RezCoreError> {
        (**self).get_repo_root()
    }

    fn is_clean(&self) -> Result<bool, RezCoreError> {
        (**self).is_clean()
    }

    fn get_current_branch(&self) -> Result<String, RezCoreError> {
        (**self).get_current_branch()
    }

    fn get_latest_commit(&self) -> Result<String, RezCoreError> {
        (**self).get_latest_commit()
    }

    fn tag_exists(&self, tag: &str) -> Result<bool, RezCoreError> {
        (**self).tag_exists(tag)
    }

    fn create_tag(&self, tag: &str, message: &str) -> Result<(), RezCoreError> {
        (**self).create_tag(tag, message)
    }

    fn create_release_tag(&self, tag: &str, message: Option<&str>) -> Result<(), RezCoreError> {
        (**self).create_release_tag(tag, message)
    }

    fn get_changelog(
        &self,
        previous_revision: Option<&str>,
        max_revisions: Option<usize>,
    ) -> Result<String, RezCoreError> {
        (**self).get_changelog(previous_revision, max_revisions)
    }

    fn get_metadata(&self) -> Result<VCSMetadata, RezCoreError> {
        (**self).get_metadata()
    }

    fn validate_repo_state(&self) -> Result<(), RezCoreError> {
        (**self).validate_repo_state()
    }

    fn is_releasable_branch(&self) -> Result<Option<bool>, RezCoreError> {
        (**self).is_releasable_branch()
    }
}

/// Detect the nearest supported VCS repository containing `repo_path`.
///
/// The search starts at `repo_path` and walks toward the filesystem root. If
/// multiple VCS markers occur at the same depth, the result is ambiguous and
/// the caller must choose a repository explicitly.
pub fn detect_vcs(
    repo_path: &Path,
) -> Result<Option<Box<dyn ReleaseVCS + Send + Sync>>, RezCoreError> {
    let canonical_path =
        std::fs::canonicalize(repo_path).unwrap_or_else(|_| repo_path.to_path_buf());
    let mut current = if canonical_path.is_file() {
        canonical_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(canonical_path)
    } else {
        canonical_path
    };

    loop {
        let markers: Vec<&str> = [
            ("git", current.join(".git").exists()),
            ("hg", current.join(".hg").exists()),
            ("svn", current.join(".svn").exists()),
        ]
        .into_iter()
        .filter_map(|(name, found)| found.then_some(name))
        .collect();

        if markers.len() > 1 {
            return Err(RezCoreError::ReleaseVCSError(format!(
                "Ambiguous VCS repositories at '{}': {}",
                current.display(),
                markers.join(", ")
            )));
        }

        if let Some(marker) = markers.first() {
            let repository = match *marker {
                "git" => {
                    #[cfg(feature = "git")]
                    {
                        Box::new(git::GitVCS::new(current.clone())?)
                            as Box<dyn ReleaseVCS + Send + Sync>
                    }
                    #[cfg(not(feature = "git"))]
                    {
                        return Ok(None);
                    }
                }
                "hg" => Box::new(hg::MercurialVCS::new(current.clone())?)
                    as Box<dyn ReleaseVCS + Send + Sync>,
                "svn" => Box::new(svn::SvnVCS::new(current.clone())?)
                    as Box<dyn ReleaseVCS + Send + Sync>,
                _ => unreachable!("VCS marker list only contains supported markers"),
            };
            return Ok(Some(repository));
        }

        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }

    Ok(None)
}

/// Get VCS metadata for a package repository
pub fn get_vcs_metadata(repo_path: &Path) -> Result<Option<VCSMetadata>, RezCoreError> {
    if let Some(vcs) = detect_vcs(repo_path)? {
        let metadata = vcs.get_metadata()?;
        Ok(Some(metadata))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "git")]
    use git2;
    use std::fs;

    #[test]
    fn test_stub_vcs_creation() {
        let vcs = StubVCS::new(PathBuf::from("/tmp/repo"));
        assert_eq!(vcs.get_type_name(), "stub");
        assert!(vcs.is_clean().unwrap());
        assert_eq!(vcs.get_current_branch().unwrap(), "main");
    }

    #[test]
    fn test_stub_vcs_metadata() {
        let vcs = StubVCS::new(PathBuf::from("/tmp/repo"));
        let metadata = vcs.get_metadata().unwrap();
        assert_eq!(metadata.vcs_type, "stub");
        assert_eq!(metadata.commit_hash, "stub-commit-hash");
    }

    #[test]
    fn test_vcs_metadata_default() {
        let metadata = VCSMetadata::default();
        assert_eq!(metadata.vcs_type, "");
        assert_eq!(metadata.commit_hash, "");
        assert!(metadata.extra.is_empty());
    }

    #[test]
    fn test_stub_vcs_tag_operations() {
        let vcs = StubVCS::new(PathBuf::from("/tmp/repo"));

        // Tag should not exist in stub
        assert!(!vcs.tag_exists("v1.0.0").unwrap());

        // Creating tag should succeed
        assert!(vcs.create_tag("v1.0.0", "Release 1.0.0").is_ok());
    }

    #[test]
    fn test_stub_vcs_changelog() {
        let vcs = StubVCS::new(PathBuf::from("/tmp/repo"));
        let changelog = vcs.get_changelog(None, None).unwrap();
        assert!(changelog.contains("Stub changelog"));
    }

    #[test]
    fn test_root_discovery_preserves_parent_distance() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repository = temp_dir.path().join("repository");
        let package = repository.join("packages").join("demo");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir(repository.join(".hg")).unwrap();

        let found = MercurialVCS::find_vcs_root(&package).unwrap();
        assert_eq!(&found.0, &std::fs::canonicalize(&repository).unwrap());
        assert_eq!(found.1, 2);
        assert_eq!(
            <MercurialVCS as ReleaseVCS>::find_vcs_root(&package),
            Some(found)
        );
    }

    #[test]
    fn test_root_discovery_can_check_only_the_requested_path() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repository = temp_dir.path().join("repository");
        let package = repository.join("packages").join("demo");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir(repository.join(".svn")).unwrap();

        assert!(
            find_vcs_root(&package, false, SvnVCS::is_valid_root).is_none(),
            "a non-parent search must not select an ancestor repository"
        );
    }

    #[test]
    fn test_release_vcs_error_is_distinct_from_build_error() {
        let error = RezCoreError::ReleaseVCSError("repository is dirty".to_string());
        assert!(error.to_string().contains("Release VCS error"));
        assert!(!matches!(error, RezCoreError::BuildError(_)));
    }

    #[test]
    fn test_detect_vcs_no_vcs() {
        let path = PathBuf::from("/tmp/non-repo");
        let result = detect_vcs(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_stub_marker_is_never_auto_detected() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(temp_dir.path().join(".stub"), "").unwrap();

        assert!(detect_vcs(temp_dir.path()).unwrap().is_none());
    }

    #[test]
    fn test_stub_vcs_with_metadata() {
        let metadata = VCSMetadata {
            vcs_type: "stub".to_string(),
            commit_hash: "custom-hash".to_string(),
            branch: Some("develop".to_string()),
            ..Default::default()
        };
        let vcs = StubVCS::with_metadata(PathBuf::from("/tmp/repo"), metadata);
        let retrieved = vcs.get_metadata().unwrap();
        assert_eq!(retrieved.branch, Some("develop".to_string()));
        assert_eq!(retrieved.commit_hash, "custom-hash");
    }

    /// Helper function to create a temporary git repository for testing
    #[cfg(feature = "git")]
    fn create_temp_git_repo() -> (tempfile::TempDir, git2::Repository) {
        let temp_dir = tempfile::TempDir::new().unwrap();

        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        let repo = git2::Repository::init_opts(temp_dir.path(), &opts).unwrap();

        // Configure this temporary repository only; CI runs tests in parallel.
        let mut repo_config = repo.config().unwrap();
        repo_config.set_str("user.name", "Test User").unwrap();
        repo_config
            .set_str("user.email", "test@example.com")
            .unwrap();

        // Create an initial commit
        let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };

        // Create commit in a separate scope so tree is dropped before we return repo
        {
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "Initial commit", &tree, &[])
                .unwrap();
        }

        (temp_dir, repo)
    }

    #[cfg(feature = "git")]
    fn create_temp_git_repo_with_upstream()
    -> (tempfile::TempDir, tempfile::TempDir, git2::Repository) {
        let (local_dir, repo) = create_temp_git_repo();
        let remote_dir = tempfile::TempDir::new().unwrap();
        let _remote_repo = git2::Repository::init_bare(remote_dir.path()).unwrap();
        let remote_url = remote_dir.path().to_string_lossy().into_owned();
        repo.remote_with_fetch("origin", &remote_url, "+refs/heads/*:refs/remotes/origin/*")
            .unwrap();
        repo.find_remote("origin")
            .unwrap()
            .push(&["refs/heads/main:refs/heads/main"], None)
            .unwrap();
        repo.find_branch("main", git2::BranchType::Local)
            .unwrap()
            .set_upstream(Some("origin/main"))
            .unwrap();
        (local_dir, remote_dir, repo)
    }

    #[cfg(feature = "git")]
    fn create_commit(repo: &git2::Repository, message: &str) {
        let signature = git2::Signature::now("Test User", "test@example.com").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &[&parent],
        )
        .unwrap();
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_creation() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs = git::GitVCS::new(repo_root.clone()).unwrap();
        assert_eq!(vcs.get_type_name(), "git");
        assert_eq!(vcs.get_repo_root().unwrap(), repo_root);
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_is_clean() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs = git::GitVCS::new(repo_root.clone()).unwrap();

        // Fresh repo should be clean
        assert!(vcs.is_clean().unwrap());

        // Create a file - should make repo dirty
        fs::write(repo_root.join("test.txt"), "test content").unwrap();
        assert!(!vcs.is_clean().unwrap());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_get_current_branch() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs = git::GitVCS::new(repo_root.clone()).unwrap();

        // Default branch should be "main"
        let branch = vcs.get_current_branch().unwrap();
        assert_eq!(branch, "main");
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_get_latest_commit() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs = git::GitVCS::new(repo_root.clone()).unwrap();

        let commit_hash = vcs.get_latest_commit().unwrap();
        // Should be a valid 40-char hex string
        assert_eq!(commit_hash.len(), 40);
        assert!(commit_hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_tag_operations() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs = git::GitVCS::new(repo_root.clone()).unwrap();

        // Tag should not exist initially
        assert!(!vcs.tag_exists("v1.0.0").unwrap());

        // Create tag
        vcs.create_tag("v1.0.0", "Release 1.0.0").unwrap();

        // Tag should exist now
        assert!(vcs.tag_exists("v1.0.0").unwrap());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_get_metadata() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs = git::GitVCS::new(repo_root.clone()).unwrap();

        let metadata = vcs.get_metadata().unwrap();
        assert_eq!(metadata.vcs_type, "git");
        assert_eq!(metadata.branch, Some("main".to_string()));
        assert_eq!(metadata.commit_hash.len(), 40);
        assert_eq!(metadata.author_name, Some("Test User".to_string()));
        assert_eq!(metadata.author_email, Some("test@example.com".to_string()));
        assert!(metadata.timestamp.is_some());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_changelog() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs = git::GitVCS::new(repo_root.clone()).unwrap();

        // Get changelog
        let changelog = vcs.get_changelog(None, None).unwrap();
        assert!(changelog.contains("Changelog from"));
        assert!(changelog.contains("Initial commit"));
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_not_a_repo() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let result = git::GitVCS::new(temp_dir.path().to_path_buf());
        assert!(result.is_err());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_get_tracking_branch_none() {
        // Create a repo with no upstream branch
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        // No upstream set, should return None
        let result = vcs.get_tracking_branch(&repo, Some("main"));
        assert!(result.is_none());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_get_tracking_branch_no_branch() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        // No branch name provided, should return None
        let result = vcs.get_tracking_branch(&repo, None);
        assert!(result.is_none());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_get_push_url_no_remote() {
        // Create a repo with no remote
        let temp_dir = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(temp_dir.path()).unwrap();

        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        // No remote set, should return None
        let result = vcs.get_push_url(&repo, "origin");
        assert!(result.is_none());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_metadata_has_new_fields() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        let metadata = vcs.get_metadata().unwrap();
        // The new fields should be present (may be None if no remote/upstream configured)
        // Just verify the structure is correct and doesn't panic
        assert_eq!(metadata.vcs_type, "git");
        assert!(!metadata.commit_hash.is_empty());
        // tracking_branch and push_url may be None (no upstream/remote configured)
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_detect_vcs_git() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();

        let vcs_opt = detect_vcs(&repo_root).unwrap();
        assert!(vcs_opt.is_some());
        assert_eq!(vcs_opt.unwrap().get_type_name(), "git");
    }

    #[test]
    fn test_detect_vcs_searches_parent_directories() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let package_dir = temp_dir.path().join("packages").join("demo");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::create_dir(temp_dir.path().join(".hg")).unwrap();

        let vcs = detect_vcs(&package_dir).unwrap().unwrap();

        assert_eq!(vcs.get_type_name(), "hg");
    }

    #[test]
    fn test_detect_vcs_prefers_the_nearest_ancestor() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let nested_repo = temp_dir.path().join("nested");
        let package_dir = nested_repo.join("packages").join("demo");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::create_dir(temp_dir.path().join(".git")).unwrap();
        std::fs::create_dir(nested_repo.join(".svn")).unwrap();

        let vcs = detect_vcs(&package_dir).unwrap().unwrap();

        assert_eq!(vcs.get_type_name(), "svn");
    }

    #[test]
    fn test_detect_vcs_reports_ambiguous_markers() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let package_dir = temp_dir.path().join("packages").join("demo");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::create_dir(temp_dir.path().join(".git")).unwrap();
        std::fs::create_dir(temp_dir.path().join(".hg")).unwrap();

        let error = match detect_vcs(&package_dir) {
            Ok(_) => panic!("multiple VCS markers must be rejected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("Ambiguous VCS repositories"));
        assert!(error.to_string().contains("git"));
        assert!(error.to_string().contains("hg"));
    }

    // --- MercurialVCS tests ---

    #[test]
    fn test_mercurial_vcs_not_a_repo() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let result = hg::MercurialVCS::new(temp_dir.path().to_path_buf());
        assert!(result.is_err());
    }

    #[test]
    fn test_mercurial_vcs_creation_placeholder() {
        // Create a temp dir with .hg directory (simulates hg repo)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".hg")).unwrap();

        let result = hg::MercurialVCS::new(temp_dir.path().to_path_buf());
        // Should succeed (creation only checks for .hg directory)
        assert!(result.is_ok());
    }

    #[test]
    fn test_mercurial_vcs_type_name() {
        // Create a temp dir with .hg directory (simulates hg repo)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".hg")).unwrap();

        let vcs = hg::MercurialVCS::new(temp_dir.path().to_path_buf()).unwrap();
        assert_eq!(vcs.get_type_name(), "hg");
    }

    #[test]
    fn test_mercurial_vcs_validate_repo_state() {
        // Create a temp dir with .hg directory (simulates hg repo)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".hg")).unwrap();

        let vcs = hg::MercurialVCS::new(temp_dir.path().to_path_buf()).unwrap();
        // validate_repo_state will check if repo is clean
        // This test just ensures the method exists and returns Result
        let _result: Result<(), RezCoreError> = vcs.validate_repo_state();
    }

    #[test]
    fn test_mercurial_vcs_is_releasable_branch() {
        // Create a temp dir with .hg directory (simulates hg repo)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".hg")).unwrap();

        let vcs = hg::MercurialVCS::new(temp_dir.path().to_path_buf()).unwrap();
        // This test just ensures the method exists and returns Result
        let _result: Result<Option<bool>, RezCoreError> = vcs.is_releasable_branch();
    }

    #[test]
    fn test_detect_vcs_mercurial() {
        // Create a temp dir with .hg directory
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".hg")).unwrap();

        let vcs_opt = detect_vcs(temp_dir.path()).unwrap();
        assert!(vcs_opt.is_some());
        assert_eq!(vcs_opt.unwrap().get_type_name(), "hg");
    }

    // --- SvnVCS tests ---

    #[test]
    fn test_svn_vcs_not_a_repo() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let result = svn::SvnVCS::new(temp_dir.path().to_path_buf());
        assert!(result.is_err());
    }

    #[test]
    fn test_svn_vcs_creation_placeholder() {
        // Create a temp dir with .svn directory (simulates svn working copy)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".svn")).unwrap();

        let result = svn::SvnVCS::new(temp_dir.path().to_path_buf());
        // Should succeed (creation only checks for .svn directory)
        assert!(result.is_ok());
    }

    #[test]
    fn test_svn_vcs_type_name() {
        // Create a temp dir with .svn directory (simulates svn working copy)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".svn")).unwrap();

        let vcs = svn::SvnVCS::new(temp_dir.path().to_path_buf()).unwrap();
        assert_eq!(vcs.get_type_name(), "svn");
    }

    #[test]
    fn test_svn_vcs_validate_repo_state() {
        // Create a temp dir with .svn directory (simulates svn working copy)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".svn")).unwrap();

        let vcs = svn::SvnVCS::new(temp_dir.path().to_path_buf()).unwrap();
        // validate_repo_state will check if working copy is clean
        // This test just ensures the method exists and returns Result
        let _result: Result<(), RezCoreError> = vcs.validate_repo_state();
    }

    #[test]
    fn test_svn_vcs_is_releasable_branch() {
        // Create a temp dir with .svn directory (simulates svn working copy)
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".svn")).unwrap();

        let vcs = svn::SvnVCS::new(temp_dir.path().to_path_buf()).unwrap();
        // This test just ensures the method exists and returns Result
        let _result: Result<Option<bool>, RezCoreError> = vcs.is_releasable_branch();
    }

    #[test]
    fn test_detect_vcs_svn() {
        // Create a temp dir with .svn directory
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(temp_dir.path().join(".svn")).unwrap();

        let vcs_opt = detect_vcs(temp_dir.path()).unwrap();
        assert!(vcs_opt.is_some());
        assert_eq!(vcs_opt.unwrap().get_type_name(), "svn");
    }

    // --- Validate repo state and is_releasable_branch tests for all VCS ---

    #[test]
    fn test_stub_vcs_validate_repo_state() {
        let vcs = StubVCS::new(PathBuf::from("/tmp/repo"));
        // StubVCS should always pass validation (default implementation checks is_clean, which returns true)
        assert!(vcs.validate_repo_state().is_ok());
    }

    #[test]
    fn test_stub_vcs_is_releasable_branch() {
        let vcs = StubVCS::new(PathBuf::from("/tmp/repo"));
        // Default implementation returns Ok(None)
        let result = vcs.is_releasable_branch().unwrap();
        assert_eq!(result, None);
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_validate_repo_state() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        // Rez's default policy requires an upstream branch.
        let error = vcs.validate_repo_state().unwrap_err().to_string();
        assert!(
            error.contains("no upstream branch"),
            "unexpected error: {error}"
        );
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_validate_repo_state_accepts_clean_synced_upstream() {
        let (_local_dir, _remote_dir, repo) = create_temp_git_repo_with_upstream();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        assert!(vcs.validate_repo_state().is_ok());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_validate_repo_state_rejects_local_commits_ahead_of_upstream() {
        let (_local_dir, _remote_dir, repo) = create_temp_git_repo_with_upstream();
        create_commit(&repo, "local commit");
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        let error = vcs.validate_repo_state().unwrap_err().to_string();
        assert!(
            error.contains("ahead of origin/main"),
            "unexpected error: {error}"
        );
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_validate_repo_state_allows_configured_no_upstream() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::with_release_settings(repo_root, true, vec![]).unwrap();

        assert!(vcs.validate_repo_state().is_ok());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_validate_repo_state_enforces_configured_branch_patterns() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature/test", &commit, false).unwrap();
        repo.set_head("refs/heads/feature/test").unwrap();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::with_release_settings(
            repo_root.clone(),
            true,
            vec!["^release/".to_string()],
        )
        .unwrap();

        let error = vcs.validate_repo_state().unwrap_err().to_string();
        assert!(
            error.contains("current branch is feature/test"),
            "unexpected error: {error}"
        );

        let vcs =
            git::GitVCS::with_release_settings(repo_root, true, vec!["^feature/".to_string()])
                .unwrap();
        assert!(vcs.validate_repo_state().is_ok());
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_rejects_invalid_branch_pattern() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::with_release_settings(repo_root, true, vec!["[invalid".to_string()])
            .unwrap();

        let error = vcs.validate_repo_state().unwrap_err().to_string();
        assert!(
            error.contains("invalid releasable branch pattern"),
            "unexpected error: {error}"
        );
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_rejects_bare_repository_during_validation() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        git2::Repository::init_bare(temp_dir.path()).unwrap();
        let vcs = git::GitVCS::new(temp_dir.path().to_path_buf()).unwrap();

        let error = vcs.validate_repo_state().unwrap_err().to_string();
        assert!(
            error.contains("bare git repository"),
            "unexpected error: {error}"
        );
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_is_releasable_branch() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs = git::GitVCS::new(repo_root).unwrap();

        // Default branch should be releasable
        let result = vcs.is_releasable_branch().unwrap();
        assert_eq!(result, Some(true));
    }

    #[cfg(feature = "git")]
    #[test]
    fn test_git_vcs_is_releasable_branch_uses_configured_patterns() {
        let (_temp_dir, repo) = create_temp_git_repo();
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature/test", &commit, false).unwrap();
        repo.set_head("refs/heads/feature/test").unwrap();
        let repo_root = repo.workdir().unwrap().to_path_buf();
        let vcs =
            git::GitVCS::with_release_settings(repo_root, true, vec!["^release/".to_string()])
                .unwrap();

        assert_eq!(vcs.is_releasable_branch().unwrap(), Some(false));
    }
}
