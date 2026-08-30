//! Stable installation discovery for IDEs and toolchain integrations.

use std::env;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use clap::Args;
use rez_next_common::{config::RezCoreConfig, error::RezCoreResult};
use serde::Serialize;

use super::bind::detect_system_tool;

/// Arguments for `rez-next info`.
#[derive(Args, Clone, Debug)]
pub struct InfoArgs {
    /// Emit the stable machine-readable schema.
    #[arg(long)]
    pub json: bool,
}

/// Machine-readable rez-next installation and configuration discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallationInfo {
    pub rez_next_version: String,
    pub rez_legacy_path: Option<PathBuf>,
    pub rez_legacy_available: bool,
    pub packages_path: Vec<PathBuf>,
    pub local_packages_path: PathBuf,
    pub release_packages_path: PathBuf,
    pub config_file: Option<PathBuf>,
    pub python_version: Option<String>,
}

impl InstallationInfo {
    /// Detect the current installation without requiring legacy Rez or Python.
    #[must_use]
    pub fn detect() -> Self {
        let config = RezCoreConfig::load();
        let rez_legacy_path = find_on_path("rez").filter(|path| !is_rez_next_executable(path));
        let python_version = detect_system_tool("python").map(|tool| tool.version);

        Self {
            rez_next_version: env!("CARGO_PKG_VERSION").to_string(),
            rez_legacy_available: rez_legacy_path.is_some(),
            rez_legacy_path,
            packages_path: config
                .packages_path
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            local_packages_path: PathBuf::from(config.local_packages_path),
            release_packages_path: PathBuf::from(config.release_packages_path),
            config_file: RezCoreConfig::get_sourced_paths().into_iter().next(),
            python_version,
        }
    }
}

/// Execute `rez-next info`.
pub fn execute(args: InfoArgs) -> RezCoreResult<()> {
    let info = InstallationInfo::detect();
    if args.json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        println!("rez-next version: {}", info.rez_next_version);
        println!(
            "Legacy Rez: {}",
            info.rez_legacy_path.as_deref().map_or_else(
                || "not found".to_string(),
                |path| path.display().to_string()
            )
        );
        println!("Package paths:");
        for path in &info.packages_path {
            println!("  {}", path.display());
        }
        println!(
            "Local packages path: {}",
            info.local_packages_path.display()
        );
        println!(
            "Release packages path: {}",
            info.release_packages_path.display()
        );
    }
    Ok(())
}

fn is_rez_next_executable(path: &Path) -> bool {
    let Ok(current) = env::current_exe() else {
        return false;
    };
    same_file_contents(path, &current)
}

fn same_file_contents(left: &Path, right: &Path) -> bool {
    const BUFFER_SIZE: usize = 64 * 1024;

    let Ok(left_metadata) = left.metadata() else {
        return false;
    };
    let Ok(right_metadata) = right.metadata() else {
        return false;
    };
    if left_metadata.len() != right_metadata.len() {
        return false;
    }

    let Ok(mut left_file) = File::open(left) else {
        return false;
    };
    let Ok(mut right_file) = File::open(right) else {
        return false;
    };
    let mut left_buffer = [0_u8; BUFFER_SIZE];
    let mut right_buffer = [0_u8; BUFFER_SIZE];
    loop {
        let Ok(left_read) = left_file.read(&mut left_buffer) else {
            return false;
        };
        let Ok(right_read) = right_file.read(&mut right_buffer) else {
            return false;
        };
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return false;
        }
        if left_read == 0 {
            return true;
        }
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let candidates = executable_names(name);

    env::split_paths(&path)
        .flat_map(|directory| {
            candidates
                .iter()
                .map(move |candidate| directory.join(candidate))
        })
        .find(|candidate| is_executable(candidate))
}

#[cfg(windows)]
fn executable_names(name: &str) -> Vec<String> {
    let extensions = env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    extensions
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| format!("{name}{extension}"))
        .collect()
}

#[cfg(not(windows))]
fn executable_names(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}
