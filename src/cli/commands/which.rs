//! Executable discovery for shell and IDE integrations.

use clap::Args;
use rez_next_common::{RezCoreError, error::RezCoreResult};

use super::info::InstallationInfo;

/// Arguments for `rez-next which`.
#[derive(Args, Clone, Debug)]
pub struct WhichArgs {
    /// Locate legacy Rez instead of rez-next.
    #[arg(long)]
    pub rez: bool,
}

/// Execute `rez-next which`.
pub fn execute(args: WhichArgs) -> RezCoreResult<()> {
    let path = if args.rez {
        InstallationInfo::detect().rez_legacy_path.ok_or_else(|| {
            RezCoreError::CliError("Legacy Rez executable was not found on PATH".to_string())
        })?
    } else {
        std::env::current_exe().map_err(RezCoreError::Io)?
    };
    println!("{}", path.display());
    Ok(())
}
