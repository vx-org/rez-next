//! Convert a YAML package definition to Rez's Python package format.

use clap::Args;
use rez_next_common::{RezCoreError, error::RezCoreResult};
use rez_next_package::{PackageFormat, PackageSerializer};
use std::path::{Path, PathBuf};

/// Arguments for the `rez yaml2py` command.
#[derive(Args, Clone, Debug)]
pub struct Yaml2PyArgs {
    /// YAML package file or directory containing package.yaml/package.yml.
    pub path: Option<PathBuf>,

    /// Increase diagnostic verbosity.
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// Convert the selected YAML package definition and print package.py to stdout.
pub fn execute(args: Yaml2PyArgs) -> RezCoreResult<()> {
    let requested = args
        .path
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let input = find_yaml_definition(&requested)?;
    let package = PackageSerializer::load_from_file(&input).map_err(|error| {
        RezCoreError::PackageParse(format!(
            "Couldn't load the package at '{}': {error}",
            requested.display()
        ))
    })?;
    let output = PackageSerializer::save_to_string(&package, PackageFormat::Python)?;
    print!("{output}");
    Ok(())
}

fn find_yaml_definition(path: &Path) -> RezCoreResult<PathBuf> {
    if path.is_file() {
        let is_yaml = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("yaml" | "yml")
        );
        if is_yaml {
            return Ok(path.to_path_buf());
        }
        return Err(RezCoreError::PackageParse(format!(
            "yaml2py expects a YAML package definition, got '{}'",
            path.display()
        )));
    }

    if path.is_dir() {
        for filename in ["package.yaml", "package.yml"] {
            let candidate = path.join(filename);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(RezCoreError::PackageParse(format!(
        "No package.yaml or package.yml found at '{}'",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn finds_yaml_with_package_priority() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("package.yml"), "name: yml").unwrap();
        fs::write(temp.path().join("package.yaml"), "name: yaml").unwrap();

        assert_eq!(
            find_yaml_definition(temp.path()).unwrap(),
            temp.path().join("package.yaml")
        );
    }

    #[test]
    fn rejects_non_yaml_input() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("package.py");
        fs::write(&path, "name = 'python'").unwrap();

        assert!(find_yaml_definition(&path).is_err());
    }
}
