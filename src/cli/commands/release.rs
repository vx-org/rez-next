//! # Release Command
//!
//! Build a package from source and deploy it to the release repository.
//! Implements VCS validation, build, tag creation, and deployment.

use clap::Args;
use rez_next_build::{GitVCS, MercurialVCS, ReleaseManager, ReleaseMode, ReleaseVCS, SvnVCS};
use rez_next_common::{RezCoreError, error::RezCoreResult};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Release command configuration
#[derive(Debug, Clone, Args)]
pub struct ReleaseArgs {
    /// Release message
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    /// Message file path
    #[arg(short = 'F', long = "message-file")]
    pub message_file: Option<PathBuf>,

    /// VCS type to use (git, hg, or svn)
    #[arg(long = "vcs", value_name = "VCS")]
    pub vcs: Option<String>,

    /// Build system type
    #[arg(long = "buildsys")]
    pub buildsys: Option<String>,

    /// Build process type
    #[arg(long = "process", default_value = "local")]
    pub process: String,

    /// Skip repository error checks
    #[arg(long = "skip-repo-errors")]
    pub skip_repo_errors: bool,

    /// Ignore existing tag
    #[arg(long = "ignore-existing-tag")]
    pub ignore_existing_tag: bool,

    /// Don't ensure latest version
    #[arg(long = "no-latest")]
    pub no_latest: bool,

    /// Specific variants to release (comma-separated indices)
    #[arg(long = "variants")]
    pub variants: Option<String>,

    /// Working directory
    #[arg(short = 'w', long = "working-dir")]
    pub working_dir: Option<PathBuf>,

    /// Dry run — show what would happen without doing it
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Verbose output
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
}

/// Execute the release command
pub fn execute(args: ReleaseArgs) -> RezCoreResult<()> {
    let working_dir = args
        .working_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    if args.verbose {
        println!("Working directory: {}", working_dir.display());
    }

    let message = args
        .message
        .clone()
        .or_else(|| read_message_file(args.message_file.as_ref()))
        .map(|value| value.to_string());

    let selected_variants = parse_variant_indices(args.variants.as_deref())?;
    let vcs = args
        .vcs
        .as_deref()
        .map(|vcs_type| {
            ensure_supported_vcs(vcs_type)?;
            create_vcs_backend(vcs_type, &working_dir)
                .map(|backend| Arc::from(backend) as Arc<dyn ReleaseVCS + Send + Sync>)
        })
        .transpose()?;

    let mode = if args.dry_run {
        ReleaseMode::DryRun
    } else if args.process == "local" {
        ReleaseMode::Local
    } else {
        ReleaseMode::Release
    };
    let mut manager = ReleaseManager::new(mode, false, false);
    manager.set_skip_vcs_validation(args.skip_repo_errors);
    manager.set_ignore_existing_tag(Some(args.ignore_existing_tag));
    manager.set_variants(selected_variants);

    if args.verbose {
        println!("VCS: {}", args.vcs.as_deref().unwrap_or("auto-detect"));
    }

    let result = manager.release_with_vcs(&working_dir, message.as_deref(), vcs)?;
    for warning in &result.warnings {
        eprintln!("Warning: {warning}");
    }
    if !result.errors.is_empty() {
        return Err(RezCoreError::BuildError(result.errors.join("; ")));
    }
    if result.success {
        println!(
            "Released {}-{} successfully!",
            result.package_name, result.version
        );
    }
    Ok(())
}

fn parse_variant_indices(selected: Option<&str>) -> RezCoreResult<Option<Vec<usize>>> {
    let Some(selected) = selected else {
        return Ok(None);
    };
    let values = selected
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| RezCoreError::BuildError(format!("Invalid variant index '{value}'")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(values))
}

fn ensure_supported_vcs(vcs_type: &str) -> RezCoreResult<()> {
    if matches!(vcs_type, "git" | "hg" | "svn") {
        return Ok(());
    }
    Err(RezCoreError::BuildError(format!(
        "Release VCS '{vcs_type}' is not supported; choose git, hg, or svn"
    )))
}

fn create_vcs_backend(
    vcs_type: &str,
    working_dir: &Path,
) -> RezCoreResult<Box<dyn ReleaseVCS + Send + Sync>> {
    match vcs_type {
        "git" => GitVCS::new(working_dir.to_path_buf())
            .map(|vcs| Box::new(vcs) as Box<dyn ReleaseVCS + Send + Sync>)
            .map_err(|error| RezCoreError::BuildError(error.to_string())),
        "hg" => MercurialVCS::new(working_dir.to_path_buf())
            .map(|vcs| Box::new(vcs) as Box<dyn ReleaseVCS + Send + Sync>)
            .map_err(|error| RezCoreError::BuildError(error.to_string())),
        "svn" => SvnVCS::new(working_dir.to_path_buf())
            .map(|vcs| Box::new(vcs) as Box<dyn ReleaseVCS + Send + Sync>)
            .map_err(|error| RezCoreError::BuildError(error.to_string())),
        _ => Err(RezCoreError::BuildError(format!(
            "Release VCS '{vcs_type}' is not supported"
        ))),
    }
}

/// Read release message from file
fn read_message_file(path: Option<&PathBuf>) -> Option<String> {
    path.and_then(|p| std::fs::read_to_string(p).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_variant_indices_selects_requested_variants() {
        assert_eq!(
            parse_variant_indices(Some("2,0")).unwrap(),
            Some(vec![2, 0])
        );
    }

    #[test]
    fn test_parse_variant_indices_rejects_invalid_values() {
        let error = parse_variant_indices(Some("missing"))
            .expect_err("invalid release variants must not be skipped");
        assert!(
            error.to_string().contains("Invalid variant index"),
            "{error}"
        );
    }

    #[test]
    fn test_release_rejects_vcs_backends_without_tag_implementation() {
        assert!(ensure_supported_vcs("git").is_ok());
        for vcs in ["unknown"] {
            let error = ensure_supported_vcs(vcs)
                .expect_err("an unsupported VCS must not report a successful release");
            assert!(error.to_string().contains(vcs), "{error}");
        }
        assert!(ensure_supported_vcs("hg").is_ok());
        assert!(ensure_supported_vcs("svn").is_ok());
    }
}
