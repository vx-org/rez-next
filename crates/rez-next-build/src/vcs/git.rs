//! Git VCS implementation
//!
//! This module provides the `GitVCS` implementation of the `ReleaseVCS` trait.

#[cfg(feature = "git")]
use git2;
#[cfg(feature = "git")]
use regex::Regex;
#[cfg(feature = "git")]
use rez_next_common::{RezCoreError, config::RezCoreConfig};
use std::path::PathBuf;

#[cfg(feature = "git")]
#[derive(Debug)]
pub struct GitVCS {
    /// Repository root path
    repo_root: PathBuf,
    release_settings: Option<GitReleaseSettings>,
}

#[cfg(feature = "git")]
#[derive(Clone, Debug, Default)]
pub(crate) struct GitReleaseSettings {
    allow_no_upstream: bool,
    releasable_branches: Vec<String>,
}

#[cfg(feature = "git")]
impl GitVCS {
    /// Return whether `path` contains a Git working tree.
    pub fn is_valid_root(path: &std::path::Path) -> bool {
        path.join(".git").exists()
    }

    /// Git repositories may be discovered in parent directories.
    pub fn search_parents_for_root() -> bool {
        true
    }

    /// Find the nearest Git root and preserve its parent distance.
    pub fn find_vcs_root(path: &std::path::Path) -> Option<(PathBuf, usize)> {
        super::find_vcs_root(path, Self::search_parents_for_root(), Self::is_valid_root)
    }

    /// Find the Git executable in `PATH`.
    pub fn find_executable(name: &str) -> Result<PathBuf, RezCoreError> {
        super::find_vcs_executable(name)
    }

    /// Create a new GitVCS
    pub fn new(repo_root: PathBuf) -> Result<Self, RezCoreError> {
        // Keep marker-based discovery compatible with callers that need
        // validation to report an invalid repository. A bare repository has
        // no `.git` marker, so accept it when libgit2 can open the path and
        // let `validate_repo_state` report the release-specific error.
        if !repo_root.join(".git").exists() && git2::Repository::open(&repo_root).is_err() {
            return Err(RezCoreError::ReleaseVCSError(
                "Not a git repository".to_string(),
            ));
        }

        Ok(Self {
            repo_root,
            release_settings: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_release_settings(
        repo_root: PathBuf,
        allow_no_upstream: bool,
        releasable_branches: Vec<String>,
    ) -> Result<Self, RezCoreError> {
        let mut vcs = Self::new(repo_root)?;
        vcs.release_settings = Some(GitReleaseSettings {
            allow_no_upstream,
            releasable_branches,
        });
        Ok(vcs)
    }

    /// Get the tracking branch for the current branch
    pub(crate) fn get_tracking_branch(
        &self,
        repo: &git2::Repository,
        branch_name: Option<&str>,
    ) -> Option<String> {
        let branch_name = branch_name?;

        // Try to get the upstream branch
        if let Ok(branch) = repo.find_branch(branch_name, git2::BranchType::Local)
            && let Ok(upstream) = branch.upstream()
            && let Ok(upstream_name) = upstream.name()
        {
            return upstream_name.map(|s| s.to_string());
        }

        // Fallback: try using git command
        if let Ok(output) = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .current_dir(&self.repo_root)
            .output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let branch = stdout.trim();
            if !branch.is_empty() && !branch.contains("fatal") {
                return Some(branch.to_string());
            }
        }

        None
    }

    /// Get the push URL for the remote
    pub(crate) fn get_push_url(
        &self,
        repo: &git2::Repository,
        remote_name: &str,
    ) -> Option<String> {
        // Try to get the remote and its push URL
        if let Ok(remote) = repo.find_remote(remote_name) {
            if let Some(push_url) = remote.pushurl().ok().flatten() {
                return Some(push_url.to_string());
            }
            // Fallback to fetch URL if no push URL is set
            if let Ok(url) = remote.url() {
                return Some(url.to_string());
            }
        }

        // Fallback: try using git command
        if let Ok(output) = std::process::Command::new("git")
            .args(["remote", "get-url", "--push", remote_name])
            .current_dir(&self.repo_root)
            .output()
            && output.status.success()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let url = stdout.trim();
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }

        None
    }

    fn load_release_settings() -> Result<GitReleaseSettings, RezCoreError> {
        let config = RezCoreConfig::try_load().map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to load release VCS configuration: {}",
                e
            ))
        })?;

        let allow_no_upstream = config
            .get_field("plugins.release_vcs.git.allow_no_upstream")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);

        let releasable_branches = match config.get_field("plugins.release_vcs.releasable_branches") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .ok_or_else(|| {
                    RezCoreError::ReleaseVCSError(
                        "GitVCS: plugins.release_vcs.releasable_branches must be a list of regular expressions"
                            .to_string(),
                    )
                })?
                .iter()
                .map(|pattern| {
                    pattern.as_str().map(str::to_owned).ok_or_else(|| {
                        RezCoreError::ReleaseVCSError(
                            "GitVCS: plugins.release_vcs.releasable_branches must contain only strings"
                                .to_string(),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        };

        Ok(GitReleaseSettings {
            allow_no_upstream,
            releasable_branches,
        })
    }

    fn release_settings(&self) -> Result<GitReleaseSettings, RezCoreError> {
        self.release_settings
            .clone()
            .map_or_else(Self::load_release_settings, Ok)
    }

    fn open_repository(&self) -> Result<git2::Repository, RezCoreError> {
        git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to open repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })
    }

    fn fetch_remote(&self, repo: &git2::Repository, remote_name: &str) -> Result<(), RezCoreError> {
        let mut remote = repo.find_remote(remote_name).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to find tracking remote '{}': {}",
                remote_name, e
            ))
        })?;
        let refspecs: [&str; 0] = [];
        remote
            .fetch(&refspecs, None, Some("rez-next release validation"))
            .map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "GitVCS: failed to update tracking remote '{}': {}",
                    remote_name, e
                ))
            })
    }

    fn validate_remote_state(
        &self,
        repo: &git2::Repository,
        tracking_branch: &str,
    ) -> Result<(), RezCoreError> {
        let Some((remote_name, _branch_name)) = tracking_branch.split_once('/') else {
            return Err(RezCoreError::ReleaseVCSError(format!(
                "GitVCS: invalid tracking branch '{}', expected '<remote>/<branch>'",
                tracking_branch
            )));
        };

        self.fetch_remote(repo, remote_name)?;

        let head = repo
            .head()
            .and_then(|head| head.peel_to_commit())
            .map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "GitVCS: failed to read HEAD while validating remote state: {}",
                    e
                ))
            })?;
        let upstream_ref = format!("refs/remotes/{}", tracking_branch);
        let upstream = repo
            .find_reference(&upstream_ref)
            .and_then(|reference| reference.peel_to_commit())
            .map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "GitVCS: failed to read tracking branch '{}': {}",
                    tracking_branch, e
                ))
            })?;
        let (ahead, behind) = repo
            .graph_ahead_behind(head.id(), upstream.id())
            .map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "GitVCS: failed to compare HEAD with '{}': {}",
                    tracking_branch, e
                ))
            })?;

        match (ahead, behind) {
            (0, 0) => Ok(()),
            (ahead, 0) => Err(RezCoreError::ReleaseVCSError(format!(
                "Could not release: {} commits ahead of {}.",
                ahead, tracking_branch
            ))),
            (0, behind) => Err(RezCoreError::ReleaseVCSError(format!(
                "Could not release: {} commits behind {}.",
                behind, tracking_branch
            ))),
            (ahead, behind) => Err(RezCoreError::ReleaseVCSError(format!(
                "Could not release: {} commits ahead of and {} commits behind {}.",
                ahead, behind, tracking_branch
            ))),
        }
    }

    fn branch_matches_policy(branch: &str, patterns: &[String]) -> Result<bool, RezCoreError> {
        for pattern in patterns {
            let regex = Regex::new(pattern).map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "GitVCS: invalid releasable branch pattern '{}': {}",
                    pattern, e
                ))
            })?;
            if regex.is_match(branch) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(feature = "git")]
impl super::ReleaseVCS for GitVCS {
    fn is_valid_root(path: &std::path::Path) -> bool
    where
        Self: Sized,
    {
        GitVCS::is_valid_root(path)
    }

    fn search_parents_for_root() -> bool
    where
        Self: Sized,
    {
        GitVCS::search_parents_for_root()
    }

    fn find_vcs_root(path: &std::path::Path) -> Option<(PathBuf, usize)>
    where
        Self: Sized,
    {
        GitVCS::find_vcs_root(path)
    }

    fn find_executable(name: &str) -> Result<PathBuf, RezCoreError>
    where
        Self: Sized,
    {
        GitVCS::find_executable(name)
    }

    fn get_type_name(&self) -> &str {
        "git"
    }

    fn get_repo_root(&self) -> Result<PathBuf, RezCoreError> {
        Ok(self.repo_root.clone())
    }

    fn is_clean(&self) -> Result<bool, RezCoreError> {
        let repo = self.open_repository()?;

        let mut status_opts = git2::StatusOptions::new();
        status_opts.include_untracked(true);
        status_opts.include_ignored(false);

        let statuses = repo.statuses(Some(&mut status_opts)).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to get status for repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })?;

        // Repository is clean if there are no status entries
        Ok(statuses.is_empty())
    }

    fn validate_repo_state(&self) -> Result<(), RezCoreError> {
        let repo = self.open_repository()?;
        if repo.is_bare() {
            return Err(RezCoreError::ReleaseVCSError(
                "Could not release: bare git repository".to_string(),
            ));
        }

        let settings = self.release_settings()?;
        let branch_name = repo
            .head()
            .ok()
            .and_then(|head| head.shorthand().ok().map(str::to_owned));
        let tracking_branch = self.get_tracking_branch(&repo, branch_name.as_deref());

        if tracking_branch.is_none() && !settings.allow_no_upstream {
            return Err(RezCoreError::ReleaseVCSError(
                "Release cancelled: there is no upstream branch (git cannot see a remote - you should probably FIX THIS FIRST!). To allow the release, set the config entry 'plugins.release_vcs.git.allow_no_upstream' to true."
                    .to_string(),
            ));
        }

        if !settings.releasable_branches.is_empty() {
            let current_branch = self.get_current_branch()?;
            if !self.is_releasable_branch()?.unwrap_or(true) {
                return Err(RezCoreError::ReleaseVCSError(format!(
                    "Could not release: current branch is {}, must match one of: {}",
                    current_branch,
                    settings.releasable_branches.join(", ")
                )));
            }
        }

        if !self.is_clean()? {
            return Err(RezCoreError::ReleaseVCSError(
                "Could not release: there are uncommitted changes".to_string(),
            ));
        }

        if let Some(tracking_branch) = tracking_branch {
            self.validate_remote_state(&repo, &tracking_branch)?;
        }

        Ok(())
    }

    fn get_current_branch(&self) -> Result<String, RezCoreError> {
        let repo = git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!("Failed to open git repository: {}", e))
        })?;

        let head = repo
            .head()
            .map_err(|e| RezCoreError::ReleaseVCSError(format!("Failed to get HEAD: {}", e)))?;

        // Check if HEAD is a branch reference
        if head.is_branch()
            && let Ok(branch_name) = head.shorthand()
        {
            return Ok(branch_name.to_string());
        }

        // Detached HEAD state
        let commit = head.peel_to_commit().map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to peel to commit for repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })?;
        Ok(format!("detached-{}", &commit.id().to_string()[..8]))
    }

    fn get_latest_commit(&self) -> Result<String, RezCoreError> {
        let repo = git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to open repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })?;

        let head = repo
            .head()
            .map_err(|e| RezCoreError::ReleaseVCSError(format!("Failed to get HEAD: {}", e)))?;

        let commit = head
            .peel_to_commit()
            .map_err(|e| RezCoreError::ReleaseVCSError(format!("Failed to get commit: {}", e)))?;

        Ok(commit.id().to_string())
    }

    fn tag_exists(&self, tag: &str) -> Result<bool, RezCoreError> {
        let repo = git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to open repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })?;

        // Check if tag reference exists
        let tag_ref_name = format!("refs/tags/{}", tag);

        match repo.find_reference(&tag_ref_name) {
            Ok(_) => Ok(true),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(false),
            Err(e) => Err(RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to check tag '{}' in repository at '{}': {}",
                tag,
                self.repo_root.display(),
                e
            ))),
        }
    }

    fn create_tag(&self, tag: &str, message: &str) -> Result<(), RezCoreError> {
        let repo = git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to open repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })?;

        // Get the current HEAD commit
        let head = repo.head().map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to get HEAD for tag '{}' in repository at '{}': {}",
                tag,
                self.repo_root.display(),
                e
            ))
        })?;
        let commit = head.peel_to_commit().map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to peel to commit for tag '{}' in repository at '{}': {}",
                tag,
                self.repo_root.display(),
                e
            ))
        })?;

        // Create signature for tag
        let sig = git2::Signature::now("Rez Next Build", "rez-next@build").map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to create signature for tag '{}': {}",
                tag, e
            ))
        })?;

        // Create an annotated tag (force = false)
        let oid = commit.id();
        let obj = repo
            .find_object(oid, Some(git2::ObjectType::Commit))
            .map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "GitVCS: failed to find commit object for tag '{}' in repository at '{}': {}",
                    tag,
                    self.repo_root.display(),
                    e
                ))
            })?;

        repo.tag(tag, &obj, &sig, message, false).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to create tag '{}' in repository at '{}': {}",
                tag,
                self.repo_root.display(),
                e
            ))
        })?;

        tracing::info!("GitVCS: created tag '{}' with message '{}'", tag, message);
        Ok(())
    }

    fn create_release_tag(&self, tag: &str, message: Option<&str>) -> Result<(), RezCoreError> {
        self.create_tag(tag, message.unwrap_or_default())
    }

    fn get_changelog(
        &self,
        previous_revision: Option<&str>,
        max_revisions: Option<usize>,
    ) -> Result<String, RezCoreError> {
        let repo = git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to open repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })?;

        let from = previous_revision.unwrap_or("beginning");
        let to = "HEAD";

        // Resolve revisions to commits
        let to_obj = repo.revparse_single(to).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!("Failed to parse 'to' revision: {}", e))
        })?;

        // Walk commits from to to from
        let mut revwalk = repo.revwalk().map_err(|e| {
            RezCoreError::ReleaseVCSError(format!("Failed to create revwalk: {}", e))
        })?;
        revwalk.push(to_obj.id()).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!("Failed to push to revwalk: {}", e))
        })?;

        // Try to hide the previous revision, but keep producing a useful log
        // when it is no longer reachable from the current branch.
        if let Some(previous_revision) = previous_revision
            && let Ok(from_obj) = repo.revparse_single(previous_revision)
            && let Ok(from_commit) = from_obj.peel_to_commit()
        {
            let _ = revwalk.hide(from_commit.id());
        }

        let mut changelog = String::new();
        changelog.push_str(&format!("Changelog from {} to {}:\n", from, to));

        for id in revwalk.take(max_revisions.unwrap_or(usize::MAX)) {
            let id = id.map_err(|e| {
                RezCoreError::ReleaseVCSError(format!("Failed to walk revisions: {}", e))
            })?;
            let commit = repo.find_commit(id).map_err(|e| {
                RezCoreError::ReleaseVCSError(format!("Failed to find commit: {}", e))
            })?;

            let message = commit.message().unwrap_or("(no message)");
            let short_id = &id.to_string()[..8];
            changelog.push_str(&format!(
                "  {} {}\n",
                short_id,
                message.lines().next().unwrap_or("")
            ));
        }

        Ok(changelog)
    }

    fn get_metadata(&self) -> Result<super::VCSMetadata, RezCoreError> {
        let repo = git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!("Failed to open git repository: {}", e))
        })?;

        // Get latest commit
        let head = repo
            .head()
            .map_err(|e| RezCoreError::ReleaseVCSError(format!("Failed to get HEAD: {}", e)))?;
        let commit = head
            .peel_to_commit()
            .map_err(|e| RezCoreError::ReleaseVCSError(format!("Failed to get commit: {}", e)))?;

        // Get branch name
        let branch = if head.is_branch() {
            head.shorthand().ok().map(|s| s.to_string())
        } else {
            None
        };

        // Get author info
        let author = commit.author();
        let author_name = author.name().ok().map(|s| s.to_string());
        let author_email = author.email().ok().map(|s| s.to_string());

        // Get commit message
        let commit_message = commit.message().ok().map(|s| s.to_string());

        // Get timestamp
        let timestamp = Some(commit.time().seconds());

        // Get remote name and URL
        let remote_name = {
            if let Ok(remotes) = repo.remotes() {
                if !remotes.is_empty() {
                    remotes
                        .get(0)
                        .ok()
                        .flatten()
                        .unwrap_or("origin")
                        .to_string()
                } else {
                    "origin".to_string()
                }
            } else {
                "origin".to_string()
            }
        };

        // Try to get remote URL
        let repository_url = {
            if let Ok(remote) = repo.find_remote(&remote_name) {
                remote.url().ok().map(|s| s.to_string())
            } else {
                None
            }
        };

        // Clone repository_url before moving it into the struct
        let repository_url_clone = repository_url.clone();

        // Get tracking branch (upstream)
        let tracking_branch = self.get_tracking_branch(&repo, branch.as_deref());

        // Get push URL for the remote
        let push_url = self.get_push_url(&repo, &remote_name);

        Ok(super::VCSMetadata {
            vcs_type: "git".to_string(),
            repository_url,
            branch,
            tracking_branch,
            fetch_url: repository_url_clone,
            push_url,
            commit_hash: commit.id().to_string(),
            commit_message,
            author_name,
            author_email,
            timestamp,
            extra: std::collections::HashMap::new(),
        })
    }

    /// Get the current revision as a VCSRevision object.
    ///
    /// This aligns with Rez's `ReleaseVCS.get_current_revision()` which can return
    /// "any type (str, dict etc.)". We return a `VCSRevision` with:
    /// - `revision_type`: "git"
    /// - `revision_id`: commit hash (full 40-char hex string)
    /// - `data`: JSON object with commit details (message, author, timestamp, etc.)
    /// - `metadata`: branch name, tags, etc.
    fn get_current_revision(&self) -> Result<super::VCSRevision, RezCoreError> {
        let repo = git2::Repository::open(&self.repo_root).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "GitVCS: failed to open repository at '{}': {}",
                self.repo_root.display(),
                e
            ))
        })?;

        let head = repo.head().map_err(|e| {
            RezCoreError::ReleaseVCSError(format!("GitVCS: failed to get HEAD: {}", e))
        })?;

        let commit = head.peel_to_commit().map_err(|e| {
            RezCoreError::ReleaseVCSError(format!("GitVCS: failed to get commit: {}", e))
        })?;

        let commit_hash = commit.id().to_string();

        // Build metadata: branch name, tags pointing to this commit
        let mut metadata = std::collections::HashMap::new();

        // Add branch name
        if head.is_branch()
            && let Ok(branch_name) = head.shorthand()
        {
            metadata.insert("branch".to_string(), branch_name.to_string());
        }

        // Add tags pointing to this commit
        let mut tags = Vec::new();
        repo.tag_foreach(|oid, name| {
            if oid == commit.id() {
                // `name` is &[u8], convert to &str first
                if let Ok(name_str) = std::str::from_utf8(name) {
                    let tag_name = name_str.trim_start_matches("refs/tags/");
                    tags.push(tag_name.to_string());
                }
            }
            true
        })
        .ok();

        if !tags.is_empty() {
            metadata.insert("tags".to_string(), tags.join(","));
        }

        // Build data JSON with commit details
        let data = serde_json::json!({
            "commit_hash": commit_hash,
            "message": commit.message().unwrap_or(""),
            "author_name": commit.author().name().unwrap_or(""),
            "author_email": commit.author().email().unwrap_or(""),
            "timestamp": commit.time().seconds(),
            "parent_ids": commit.parent_ids().map(|id| id.to_string()).collect::<Vec<_>>(),
        });

        let mut revision = super::VCSRevision::with_data("git", &commit_hash, data);
        revision.metadata = metadata;

        Ok(revision)
    }

    /// Export the repository at the given revision to the given path.
    ///
    /// This aligns with Rez's `ReleaseVCS.export()` classmethod.
    /// Uses `git archive` to create an archive of the repository at the given revision,
    /// then extracts it to the target path.
    ///
    /// # Arguments
    ///
    /// * `revision` - The revision to export (as `&VCSRevision`)
    /// * `path` - The path to export to (must not exist, parent must exist)
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or `RezCoreError` on failure.
    fn export(
        &self,
        revision: &super::VCSRevision,
        path: &std::path::Path,
    ) -> Result<(), RezCoreError> {
        use std::fs;
        use std::process::Command;

        // Validate target path
        if path.exists() {
            return Err(RezCoreError::ReleaseVCSError(format!(
                "Export path '{}' already exists",
                path.display()
            )));
        }

        // Ensure parent directory exists
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            return Err(RezCoreError::ReleaseVCSError(format!(
                "Parent directory '{}' does not exist",
                parent.display()
            )));
        }

        // Create target directory
        fs::create_dir_all(path).map_err(|e| {
            RezCoreError::ReleaseVCSError(format!(
                "Failed to create export directory '{}': {}",
                path.display(),
                e
            ))
        })?;

        // Use `git archive` to export the repository at the given revision
        // Format: git archive --format=tar <revision> | tar -xf - -C <path>
        let revision_spec = &revision.revision_id;

        let git_archive = Command::new("git")
            .args(["archive", "--format=tar", revision_spec])
            .current_dir(&self.repo_root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "Failed to start `git archive` for repository at '{}': {}",
                    self.repo_root.display(),
                    e
                ))
            })?;

        // Pipe to `tar -xf - -C <path>`
        let tar_output = Command::new("tar")
            .args(["-xf", "-", "-C", &path.to_string_lossy()])
            .stdin(git_archive.stdout.unwrap())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| {
                RezCoreError::ReleaseVCSError(format!(
                    "Failed to run `tar` for export to '{}': {}",
                    path.display(),
                    e
                ))
            })?;

        if !tar_output.status.success() {
            let stderr = String::from_utf8_lossy(&tar_output.stderr);
            return Err(RezCoreError::ReleaseVCSError(format!(
                "Failed to extract archive for export to '{}': {}",
                path.display(),
                stderr
            )));
        }

        tracing::info!(
            "GitVCS: exported revision '{}' to '{}'",
            revision_spec,
            path.display()
        );

        Ok(())
    }

    fn is_releasable_branch(&self) -> Result<Option<bool>, RezCoreError> {
        let branch = self.get_current_branch()?;
        let settings = self.release_settings()?;
        if settings.releasable_branches.is_empty() {
            return Ok(Some(true));
        }
        Ok(Some(Self::branch_matches_policy(
            &branch,
            &settings.releasable_branches,
        )?))
    }
}
