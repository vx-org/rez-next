//! Copy packages through the canonical filesystem repository installer.

use super::package_transfer::{PackageTransferOptions, transfer_package};
use clap::Args;
use rez_next_common::{RezCoreError, error::RezCoreResult};
use std::path::PathBuf;

#[derive(Args, Clone, Debug)]
pub struct CpArgs {
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
    /// Variant indexes to copy. Negative indexes count from the end.
    #[arg(long = "variants", num_args = 1.., value_name = "INDEX")]
    pub variant_indices: Option<Vec<i32>>,
    /// Copy every variant. This is the default for varianted packages.
    #[arg(short = 'a', long = "all-variants")]
    pub all_variants: bool,
    /// Overwrite an existing package or variant.
    #[arg(short = 'o', long = "overwrite")]
    pub overwrite: bool,
    /// Force the copy even when package metadata is non-relocatable.
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
    /// Allow copying into an empty destination repository.
    #[arg(long = "allow-empty")]
    pub allow_empty: bool,
    /// Rename the copied package.
    #[arg(long = "rename", value_name = "NAME")]
    pub rename: Option<String>,
    /// Copy the package under a different version.
    #[arg(long = "reversion", value_name = "VERSION")]
    pub reversion: Option<String>,
    /// Copy a specific variant URI instead of a package specification.
    #[arg(long = "variant-uri", value_name = "URI")]
    pub variant_uri: Option<String>,
    /// Dry run without changing the repositories.
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,
    /// Verbose output.
    #[arg(short = 'v', long = "verbose")]
    pub verbose: bool,
    /// Retained for command compatibility. Package dependencies are never copied implicitly.
    #[arg(long = "no-deps")]
    pub no_deps: bool,
}

pub fn execute(args: CpArgs) -> RezCoreResult<()> {
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
        keep_source: true,
    }))?;
    if !args.verbose {
        println!(
            "Copied {} variant(s) to {}",
            result.variants_processed,
            result.destination_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn cp_args_expose_upstream_transfer_flags() {
        let args = CpArgs {
            source_package: Some("test-1.0.0".to_string()),
            destination_path: Some(PathBuf::from("/tmp")),
            dest_path: None,
            source_paths: vec![],
            variant_indices: Some(vec![-1]),
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
        };
        assert_eq!(args.variant_indices, Some(vec![-1]));
        assert!(args.keep_timestamp);
    }

    #[test]
    fn transfer_package_installs_only_requested_variant() {
        let source = TempDir::new().unwrap();
        let destination = TempDir::new().unwrap();
        let source_package = source.path().join("tool/1.0.0");
        std::fs::create_dir_all(source_package.join("python-3.11")).unwrap();
        std::fs::create_dir_all(source_package.join("python-3.12")).unwrap();
        std::fs::write(
            source_package.join("package.py"),
            "name = 'tool'\nversion = '1.0.0'\nvariants = [['python-3.11'], ['python-3.12']]\n",
        )
        .unwrap();
        std::fs::write(source_package.join("python-3.11/marker"), "old").unwrap();
        std::fs::write(source_package.join("python-3.12/marker"), "selected").unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime
            .block_on(transfer_package(&PackageTransferOptions {
                source_package: Some("tool-1.0.0".to_string()),
                destination_path: Some(destination.path().to_path_buf()),
                source_paths: vec![source.path().to_path_buf()],
                variant_indices: Some(vec![1]),
                allow_empty: true,
                keep_source: true,
                ..Default::default()
            }))
            .unwrap();

        let installed = destination.path().join("tool/1.0.0");
        assert_eq!(
            std::fs::read_to_string(installed.join("python-3.12/marker")).unwrap(),
            "selected"
        );
        assert!(!installed.join("python-3.11/marker").exists());
    }
}
