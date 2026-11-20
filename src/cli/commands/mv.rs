//! Move packages through the canonical filesystem repository installer.

use super::package_transfer::{PackageTransferOptions, transfer_package};
use clap::Args;
use rez_next_common::{RezCoreError, error::RezCoreResult};
use std::path::PathBuf;

#[derive(Args, Clone, Debug)]
pub struct MvArgs {
    /// Source package specification (name-version).
    #[arg(
        value_name = "SRC_PKG",
        add = clap_complete::ArgValueCompleter::new(
            crate::cli::commands::completions::package_request_candidates
        )
    )]
    pub source_package: Option<String>,
    /// Destination repository path.
    #[arg(value_name = "DEST_PATH")]
    pub destination_path: Option<PathBuf>,
    /// Destination repository path when the positional form is omitted.
    #[arg(long = "dest-path", value_name = "PATH")]
    pub dest_path: Option<PathBuf>,
    /// Source repository search paths.
    #[arg(
        short = 'p',
        long = "paths",
        visible_alias = "src-path",
        value_name = "PATH"
    )]
    pub source_paths: Vec<PathBuf>,
    /// Variant indexes to move. Negative indexes count from the end.
    #[arg(long = "variants", num_args = 1.., value_name = "INDEX")]
    pub variant_indices: Option<Vec<i32>>,
    /// Move every variant. This is the default for varianted packages.
    #[arg(short = 'a', long = "all-variants")]
    pub all_variants: bool,
    /// Overwrite an existing package or variant.
    #[arg(short = 'o', long = "overwrite")]
    pub overwrite: bool,
    /// Force the move even when package metadata is non-relocatable.
    #[arg(short = 'f', long = "force")]
    pub force: bool,
    /// Perform a shallow copy using top-level directory symlinks.
    #[arg(short = 's', long = "shallow")]
    pub shallow: bool,
    /// Follow source symlinks while copying payloads.
    #[arg(long = "follow-symlinks")]
    pub follow_symlinks: bool,
    /// Preserve source file timestamps.
    #[arg(short = 'k', long = "keep-timestamp")]
    pub keep_timestamp: bool,
    /// Allow moving into an empty destination repository.
    #[arg(long = "allow-empty")]
    pub allow_empty: bool,
    /// Rename the moved package.
    #[arg(long = "rename", value_name = "NAME")]
    pub rename: Option<String>,
    /// Move the package under a different version.
    #[arg(long = "reversion", value_name = "VERSION")]
    pub reversion: Option<String>,
    /// Move a specific variant URI instead of a package specification.
    #[arg(long = "variant-uri", value_name = "URI")]
    pub variant_uri: Option<String>,
    /// Dry run without changing the repositories.
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
    /// Verbose output.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
    /// Retained for command compatibility. Package dependencies are never moved implicitly.
    #[arg(long = "no-deps")]
    pub no_deps: bool,
    /// Keep the source package after copying it.
    #[arg(long = "keep-source")]
    pub keep_source: bool,
}

pub fn execute(args: MvArgs) -> RezCoreResult<()> {
    let destination_path = args.destination_path.or(args.dest_path);
    if destination_path.is_none() && args.rename.is_none() && args.reversion.is_none() {
        return Err(RezCoreError::RequirementParse(
            "--dest-path is required unless --rename or --reversion is used".to_string(),
        ));
    }
    let runtime = tokio::runtime::Runtime::new().map_err(RezCoreError::Io)?;
    let result = runtime.block_on(transfer_package(&PackageTransferOptions {
        source_package: args.source_package,
        variant_uri: args.variant_uri,
        destination_path,
        source_paths: args.source_paths,
        variant_indices: if args.all_variants {
            None
        } else {
            args.variant_indices
        },
        force: args.force,
        overwrite: args.overwrite,
        dry_run: args.dry_run,
        verbose: args.verbose,
        keep_timestamp: args.keep_timestamp,
        shallow: args.shallow,
        follow_symlinks: args.follow_symlinks,
        allow_empty: args.allow_empty,
        rename: args.rename,
        reversion: args.reversion,
        keep_source: args.keep_source,
    }))?;
    if !args.verbose {
        let action = if args.keep_source { "Copied" } else { "Moved" };
        println!(
            "{action} {} variant(s) to {}",
            result.variants_processed,
            result.destination_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mv_args_expose_timestamp_and_variant_controls() {
        let args = MvArgs {
            source_package: Some("test-1.0.0".to_string()),
            destination_path: Some(PathBuf::from("/tmp")),
            dest_path: None,
            source_paths: vec![],
            variant_indices: Some(vec![0]),
            all_variants: false,
            overwrite: false,
            force: false,
            shallow: false,
            follow_symlinks: false,
            keep_timestamp: true,
            allow_empty: false,
            rename: None,
            reversion: None,
            variant_uri: None,
            dry_run: false,
            verbose: false,
            no_deps: false,
            keep_source: false,
        };
        assert!(args.keep_timestamp);
        assert_eq!(args.variant_indices, Some(vec![0]));
    }
}
