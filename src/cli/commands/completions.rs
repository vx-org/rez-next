//! Stable package queries and dynamic shell completion support.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use clap::{Args, CommandFactory, Subcommand, ValueEnum};
use clap_complete::CompletionCandidate;
use clap_complete::env::Shells;
use rez_next_common::{RezCoreConfig, error::RezCoreResult};
use rez_next_repository::simple_repository::RepositoryManager;
use serde::Serialize;

use crate::cli::RezCli;
use crate::cli::utils::expand_home_path;

/// Version of the machine-readable completion response contract.
pub const COMPLETION_SCHEMA_VERSION: u32 = 1;

const COMPLETION_ENV: &str = "REZ_NEXT_COMPLETE";

/// Arguments for the stable completions command.
#[derive(Args, Clone)]
pub struct CompletionsArgs {
    /// Completion operation to perform.
    #[command(subcommand)]
    pub command: CompletionsCommand,
}

/// Stable completion operations for IDEs and shells.
#[derive(Subcommand, Clone)]
pub enum CompletionsCommand {
    /// Query package families and their versions.
    Packages(PackageCompletionArgs),
    /// Query versions for one package family.
    Versions(VersionCompletionArgs),
    /// Generate a dynamic shell registration script.
    Shell(ShellCompletionArgs),
}

/// Package-family completion query.
#[derive(Args, Clone)]
pub struct PackageCompletionArgs {
    /// Package-name prefix.
    #[arg(long, default_value = "")]
    pub prefix: String,
    /// Emit the versioned JSON response contract.
    #[arg(long)]
    pub json: bool,
    /// Repository paths to query instead of configured paths.
    #[arg(short, long)]
    pub repository: Vec<PathBuf>,
}

/// Package-version completion query.
#[derive(Args, Clone)]
pub struct VersionCompletionArgs {
    /// Package family to query.
    #[arg(
        long,
        add = clap_complete::ArgValueCompleter::new(package_request_candidates)
    )]
    pub package: String,
    /// Version prefix.
    #[arg(long, default_value = "")]
    pub prefix: String,
    /// Emit the versioned JSON response contract.
    #[arg(long)]
    pub json: bool,
    /// Repository paths to query instead of configured paths.
    #[arg(short, long)]
    pub repository: Vec<PathBuf>,
}

/// Dynamic shell registration generation.
#[derive(Args, Clone)]
pub struct ShellCompletionArgs {
    /// Target shell.
    #[arg(long, value_enum)]
    pub shell: CompletionShell,
}

/// Shells supported on every rez-next platform.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl CompletionShell {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Powershell => "powershell",
        }
    }
}

#[derive(Debug, Serialize)]
struct PackageCompletion {
    name: String,
    versions: Vec<String>,
    description: Option<String>,
}

#[derive(Debug, Serialize)]
struct PackageCompletionResponse {
    schema_version: u32,
    kind: &'static str,
    packages: Vec<PackageCompletion>,
}

#[derive(Debug, Serialize)]
struct VersionCompletionResponse {
    schema_version: u32,
    kind: &'static str,
    package: String,
    versions: Vec<String>,
}

/// Handle an environment-activated completion request before normal CLI parsing.
pub fn complete_from_env() {
    let binary_name = current_binary_name();
    clap_complete::CompleteEnv::with_factory(RezCli::command)
        .var(COMPLETION_ENV)
        .bin(binary_name)
        .complete();
}

/// Execute a stable completion operation.
pub fn execute(args: CompletionsArgs) -> RezCoreResult<()> {
    match args.command {
        CompletionsCommand::Shell(args) => generate_shell_registration(args.shell),
        CompletionsCommand::Packages(args) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let packages =
                runtime.block_on(query_packages(&args.repository, args.prefix.as_str()))?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&PackageCompletionResponse {
                        schema_version: COMPLETION_SCHEMA_VERSION,
                        kind: "packages",
                        packages,
                    })?
                );
            } else {
                for package in packages {
                    println!("{}", package.name);
                }
            }
            Ok(())
        }
        CompletionsCommand::Versions(args) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let versions = runtime.block_on(query_versions(
                &args.repository,
                args.package.as_str(),
                args.prefix.as_str(),
            ))?;
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&VersionCompletionResponse {
                        schema_version: COMPLETION_SCHEMA_VERSION,
                        kind: "versions",
                        package: args.package,
                        versions,
                    })?
                );
            } else {
                for version in versions {
                    println!("{version}");
                }
            }
            Ok(())
        }
    }
}

/// Complete package requests for clap's dynamic shell completion engine.
pub fn package_request_candidates(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(prefix) = current.to_str() else {
        return Vec::new();
    };
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return Vec::new();
    };
    runtime
        .block_on(query_request_candidates(prefix))
        .unwrap_or_default()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

fn generate_shell_registration(shell: CompletionShell) -> RezCoreResult<()> {
    let shells = Shells::builtins();
    let completer = shells.completer(shell.as_str()).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unsupported completion shell: {}", shell.as_str()),
        )
    })?;
    let binary_name = current_binary_name();
    let executable = std::env::current_exe()?;
    let executable = executable.to_string_lossy();
    let mut stdout = std::io::stdout().lock();
    completer.write_registration(
        COMPLETION_ENV,
        "rez_next",
        &binary_name,
        &executable,
        &mut stdout,
    )?;
    Ok(())
}

fn current_binary_name() -> String {
    std::env::args_os()
        .next()
        .as_deref()
        .and_then(|argument| Path::new(argument).file_stem())
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("rez-next")
        .to_string()
}

async fn query_packages(
    repositories: &[PathBuf],
    prefix: &str,
) -> RezCoreResult<Vec<PackageCompletion>> {
    let manager = repository_manager(repositories)?;
    let names = manager.list_packages().await?;
    let mut completions = Vec::new();
    for name in names.into_iter().filter(|name| name.starts_with(prefix)) {
        let packages = manager.find_packages(&name).await?;
        let description = packages
            .iter()
            .find_map(|package| package.description.clone());
        let versions = package_versions(packages.iter().map(|package| package.as_ref()), "");
        completions.push(PackageCompletion {
            name,
            versions,
            description,
        });
    }
    Ok(completions)
}

async fn query_versions(
    repositories: &[PathBuf],
    package: &str,
    prefix: &str,
) -> RezCoreResult<Vec<String>> {
    let manager = repository_manager(repositories)?;
    let packages = manager.find_packages(package).await?;
    Ok(package_versions(
        packages.iter().map(|package| package.as_ref()),
        prefix,
    ))
}

async fn query_request_candidates(prefix: &str) -> RezCoreResult<Vec<String>> {
    let manager = repository_manager(&[])?;
    let names = manager.list_packages().await?;
    let mut candidates = Vec::new();
    for name in names {
        if name.starts_with(prefix) {
            candidates.push(name.clone());
        }
        let version_prefix = format!("{name}-");
        if prefix.starts_with(&version_prefix) {
            let packages = manager.find_packages(&name).await?;
            for version in package_versions(packages.iter().map(|package| package.as_ref()), "") {
                let request = format!("{name}-{version}");
                if request.starts_with(prefix) {
                    candidates.push(request);
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

fn package_versions<'a>(
    packages: impl Iterator<Item = &'a rez_next_package::Package>,
    prefix: &str,
) -> Vec<String> {
    let mut seen = HashSet::new();
    packages
        .filter_map(|package| package.version.as_ref())
        .map(|version| version.as_str().to_string())
        .filter(|version| version.starts_with(prefix))
        .filter(|version| seen.insert(version.clone()))
        .collect()
}

fn repository_manager(repositories: &[PathBuf]) -> RezCoreResult<RepositoryManager> {
    let paths = if repositories.is_empty() {
        RezCoreConfig::load()
            .packages_path
            .into_iter()
            .map(|path| expand_home_path(&path).to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    } else {
        repositories
            .iter()
            .map(|path| {
                expand_home_path(&path.to_string_lossy())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    };

    RepositoryManager::from_package_paths(paths)
}
