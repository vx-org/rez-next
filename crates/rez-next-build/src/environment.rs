//! Build environment management

use crate::{BuildEvent, BuildEventKind, BuildStep, BuildType};
use rez_next_common::{RezCoreConfig, RezCoreError, SystemInfo, utils::get_thread_count};
use rez_next_context::ResolvedContext;
use rez_next_package::{Package, PackageInstallLayout, PackageRequirement};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;

const DEFAULT_BUILD_VERSION: &str = "0.0.0";

/// Build environment for package builds
#[derive(Debug, Clone)]
pub struct BuildEnvironment {
    /// Package being built
    package: Package,
    /// Build directory
    build_dir: PathBuf,
    /// Install directory
    install_dir: PathBuf,
    /// Temporary directory
    temp_dir: PathBuf,
    /// Environment variables
    env_vars: HashMap<String, String>,
    /// Build context (resolved dependencies)
    context: Option<ResolvedContext>,
    /// Optional live build event stream.
    event_sender: Option<mpsc::UnboundedSender<BuildEvent>>,
    /// Build ID used in live events.
    event_build_id: Option<String>,
}

impl BuildEnvironment {
    /// Create a new build environment
    pub fn new(
        package: &Package,
        base_build_dir: &PathBuf,
        context: Option<&ResolvedContext>,
    ) -> Result<Self, RezCoreError> {
        Self::with_install_path(package, base_build_dir, context, None)
    }

    /// Create a new build environment with custom install path
    pub fn with_install_path(
        package: &Package,
        base_build_dir: &PathBuf,
        context: Option<&ResolvedContext>,
        install_path: Option<&PathBuf>,
    ) -> Result<Self, RezCoreError> {
        Self::with_install_path_and_type(
            package,
            base_build_dir,
            context,
            install_path,
            BuildType::Local,
        )
    }

    /// Create a build environment with an explicit Rez build type.
    pub fn with_install_path_and_type(
        package: &Package,
        base_build_dir: &PathBuf,
        context: Option<&ResolvedContext>,
        install_path: Option<&PathBuf>,
        build_type: BuildType,
    ) -> Result<Self, RezCoreError> {
        // Normalize the base build directory to handle various path formats
        let normalized_base = Self::normalize_build_path(base_build_dir)?;
        let package_relative_path = PackageInstallLayout::package_base_relative_path(package)?;
        let package_build_dir = normalized_base.join(&package.name);

        // Use custom install path or default to build directory
        let install_dir = if let Some(custom_path) = install_path {
            custom_path.join(package_relative_path)
        } else {
            package_build_dir.join("install")
        };

        let temp_dir = package_build_dir.join("tmp");

        // Set up environment variables
        let mut env_vars = HashMap::new();

        // Standard Rez build environment variables (matching original Rez)
        // REZ_BUILD_ENV: marks this as a Rez build environment
        env_vars.insert("REZ_BUILD_ENV".to_string(), "1".to_string());

        // REZ_BUILD_TYPE: build type (local or central)
        env_vars.insert("REZ_BUILD_TYPE".to_string(), build_type.name().to_string());

        // REZ_BUILD_INSTALL: whether to install (1 or 0)
        // Default to "0"; set to "1" when install_path is provided
        let install_flag = if install_path.is_some() { "1" } else { "0" };
        env_vars.insert("REZ_BUILD_INSTALL".to_string(), install_flag.to_string());

        // Add package-specific variables
        let package_version = package
            .version
            .as_ref()
            .map(|version| version.as_str().to_string())
            .unwrap_or_else(|| DEFAULT_BUILD_VERSION.to_string());
        env_vars.insert("REZ_BUILD_PROJECT_NAME".to_string(), package.name.clone());
        env_vars.insert("REZ_BUILD_PROJECT_VERSION".to_string(), package_version);

        env_vars.insert(
            "REZ_BUILD_INSTALL_PATH".to_string(),
            install_dir.to_string_lossy().to_string(),
        );
        env_vars.insert(
            "REZ_BUILD_PATH".to_string(),
            package_build_dir.to_string_lossy().to_string(),
        );
        env_vars.insert(
            "REZ_BUILD_PROJECT_DESCRIPTION".to_string(),
            package
                .description
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string(),
        );
        env_vars.insert(
            "REZ_BUILD_THREAD_COUNT".to_string(),
            get_thread_count(RezCoreConfig::load().thread_count).to_string(),
        );

        // Add variant-related environment variables (matching original Rez)
        // These will be set when building variants
        env_vars.insert("REZ_BUILD_VARIANT_INDEX".to_string(), "0".to_string());
        env_vars.insert("REZ_BUILD_VARIANT_REQUIRES".to_string(), String::new());
        env_vars.insert("REZ_BUILD_VARIANT_SUBPATH".to_string(), String::new());
        Self::set_build_requirements(&mut env_vars, package, &[]);
        Self::set_package_file_vars(&mut env_vars, package, None);

        // Add context environment if available
        if let Some(context) = context {
            for (key, value) in &context.environment_vars {
                env_vars.insert(key.clone(), value.clone());
            }
        }

        Self::set_target_environment(&mut env_vars, &[]);

        Ok(Self {
            package: package.clone(),
            build_dir: package_build_dir,
            install_dir,
            temp_dir,
            env_vars,
            context: context.cloned(),
            event_sender: None,
            event_build_id: None,
        })
    }

    /// Get build directory
    pub fn get_build_dir(&self) -> &PathBuf {
        &self.build_dir
    }

    /// Get install directory
    pub fn get_install_dir(&self) -> &PathBuf {
        &self.install_dir
    }

    /// Get temporary directory
    pub fn get_temp_dir(&self) -> &PathBuf {
        &self.temp_dir
    }

    /// Get environment variables
    pub fn get_env_vars(&self) -> &HashMap<String, String> {
        &self.env_vars
    }

    /// Get package
    pub fn get_package(&self) -> &Package {
        &self.package
    }

    /// Get context
    pub fn get_context(&self) -> Option<&ResolvedContext> {
        self.context.as_ref()
    }

    /// Set the live build event sender.
    pub fn set_event_sender(
        &mut self,
        sender: Option<mpsc::UnboundedSender<BuildEvent>>,
        build_id: String,
    ) {
        self.event_sender = sender;
        self.event_build_id = Some(build_id);
    }

    /// Emit a live build event for this build environment.
    pub fn emit_event(&self, step: BuildStep, kind: BuildEventKind, message: String) {
        let Some(sender) = &self.event_sender else {
            return;
        };
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let _ = sender.send(BuildEvent {
            build_id: self.event_build_id.clone().unwrap_or_default(),
            step: Some(step),
            kind,
            message,
            success: None,
            duration_ms: None,
            timestamp_ms,
        });
    }

    /// Add environment variable
    pub fn add_env_var(&mut self, name: String, value: String) {
        self.env_vars.insert(name, value);
    }

    /// Remove environment variable
    pub fn remove_env_var(&mut self, name: &str) {
        self.env_vars.remove(name);
    }

    /// Set variant-related environment variables
    pub fn set_variant_env(&mut self, variant_index: usize, variant_requires: &[String]) {
        self.env_vars.insert(
            "REZ_BUILD_VARIANT_INDEX".to_string(),
            variant_index.to_string(),
        );
        self.env_vars.insert(
            "REZ_BUILD_VARIANT_REQUIRES".to_string(),
            variant_requires.join(" "),
        );
        Self::set_build_requirements(&mut self.env_vars, &self.package, variant_requires);
        Self::set_target_environment(&mut self.env_vars, variant_requires);
    }

    /// Set the variant subpath for the build environment.
    pub fn set_variant_subpath(&mut self, variant_subpath: &str) {
        self.env_vars.insert(
            "REZ_BUILD_VARIANT_SUBPATH".to_string(),
            variant_subpath.to_string(),
        );
    }

    /// Apply the Core's validated variant layout to build and installation paths.
    pub(crate) fn apply_install_layout(&mut self, layout: &PackageInstallLayout) {
        let subpath = layout.variant_subpath();
        self.build_dir = self.build_dir.join(subpath);
        self.install_dir = self.install_dir.join(subpath);
        self.temp_dir = self.temp_dir.join(subpath);
        self.env_vars.insert(
            "REZ_BUILD_PATH".to_string(),
            self.build_dir.to_string_lossy().into_owned(),
        );
        self.env_vars.insert(
            "REZ_BUILD_INSTALL_PATH".to_string(),
            self.install_dir.to_string_lossy().into_owned(),
        );
        self.set_variant_subpath(&subpath.to_string_lossy());
    }

    /// Set source-related environment variables.
    pub fn set_source_path(&mut self, source_path: &Path) {
        self.env_vars.insert(
            "REZ_BUILD_SOURCE_PATH".to_string(),
            source_path.to_string_lossy().to_string(),
        );

        let project_file = Self::project_file_for_source(&self.package, source_path);
        self.env_vars.insert(
            "REZ_BUILD_PROJECT_FILE".to_string(),
            project_file.to_string_lossy().to_string(),
        );
    }

    fn set_package_file_vars(
        env_vars: &mut HashMap<String, String>,
        package: &Package,
        source_path: Option<&Path>,
    ) {
        let Some(project_file) = source_path
            .map(|path| Self::project_file_for_source(package, path))
            .or_else(|| package.filepath.as_deref().map(PathBuf::from))
        else {
            return;
        };

        env_vars.insert(
            "REZ_BUILD_PROJECT_FILE".to_string(),
            project_file.to_string_lossy().to_string(),
        );
        if let Some(parent) = project_file.parent() {
            env_vars.insert(
                "REZ_BUILD_SOURCE_PATH".to_string(),
                parent.to_string_lossy().to_string(),
            );
        }
    }

    fn project_file_for_source(package: &Package, source_path: &Path) -> PathBuf {
        if let Some(filepath) = package.filepath.as_deref() {
            return PathBuf::from(filepath);
        }

        for filename in ["package.py", "package.yaml", "package.yml"] {
            let candidate = source_path.join(filename);
            if candidate.exists() {
                return candidate;
            }
        }

        source_path.join("package.py")
    }

    fn set_build_requirements(
        env_vars: &mut HashMap<String, String>,
        package: &Package,
        variant_requires: &[String],
    ) {
        let requirements = package
            .requires
            .iter()
            .chain(package.build_requires.iter())
            .chain(package.private_build_requires.iter())
            .chain(variant_requires.iter())
            .cloned()
            .collect::<Vec<_>>();

        let unversioned = requirements
            .iter()
            .map(|requirement| {
                PackageRequirement::parse(requirement)
                    .map(|parsed| parsed.name().to_string())
                    .unwrap_or_else(|_| requirement.clone())
            })
            .collect::<Vec<_>>();

        env_vars.insert("REZ_BUILD_REQUIRES".to_string(), requirements.join(" "));
        env_vars.insert(
            "REZ_BUILD_REQUIRES_UNVERSIONED".to_string(),
            unversioned.join(" "),
        );
    }

    fn set_target_environment(env_vars: &mut HashMap<String, String>, variant_requires: &[String]) {
        let system = SystemInfo::detect();
        let mut platform = system.platform;
        let mut arch = canonical_architecture(&system.arch);
        let mut os = system.os;

        for requirement in variant_requires {
            let Ok(parsed) = PackageRequirement::parse(requirement) else {
                continue;
            };
            let Some(value) = parsed.version_spec().map(strip_requirement_operator) else {
                continue;
            };
            if value.is_empty() {
                continue;
            }

            match parsed.name() {
                "platform" => platform = value.to_string(),
                "arch" => arch = canonical_architecture(value),
                "os" => os = value.to_string(),
                _ => {}
            }
        }

        env_vars.insert(
            "REZ_BUILD_TARGET_PLATFORM".to_string(),
            format!("{platform}-{arch}"),
        );
        env_vars.insert("REZ_BUILD_TARGET_ARCH".to_string(), arch);
        env_vars.insert("REZ_BUILD_TARGET_OS".to_string(), os);
    }

    /// Get the variant install path (for hash variants)
    pub fn get_variant_install_path(&self, variant_hash: Option<&str>) -> PathBuf {
        match variant_hash {
            Some(hash) => self.install_dir.join(hash),
            None => {
                // Non-hash variant: use index-based path
                // This will be set by the caller based on variant_index
                self.install_dir.clone()
            }
        }
    }

    /// Set up build environment directories
    pub async fn setup(&self) -> Result<(), RezCoreError> {
        // Create directories
        tokio::fs::create_dir_all(&self.build_dir)
            .await
            .map_err(|e| RezCoreError::BuildError(format!("Failed to create build dir: {}", e)))?;

        tokio::fs::create_dir_all(&self.install_dir)
            .await
            .map_err(|e| {
                RezCoreError::BuildError(format!("Failed to create install dir: {}", e))
            })?;

        tokio::fs::create_dir_all(&self.temp_dir)
            .await
            .map_err(|e| RezCoreError::BuildError(format!("Failed to create temp dir: {}", e)))?;

        Ok(())
    }

    /// Clean build environment
    pub async fn clean(&self) -> Result<(), RezCoreError> {
        if self.build_dir.exists() {
            tokio::fs::remove_dir_all(&self.build_dir)
                .await
                .map_err(|e| {
                    RezCoreError::BuildError(format!("Failed to clean build dir: {}", e))
                })?;
        }
        Ok(())
    }

    /// Get environment as shell script
    pub fn to_shell_script(&self, shell_type: &rez_next_context::ShellType) -> String {
        let environment = rez_next_rex::RexEnvironment {
            vars: self.env_vars.clone(),
            ..Default::default()
        };
        let shell_type: rez_next_common::ShellType = shell_type.clone().into();
        rez_next_rex::generate_shell_script(&environment, &shell_type)
    }

    /// Normalize build path to handle various path formats
    fn normalize_build_path(path: &PathBuf) -> Result<PathBuf, RezCoreError> {
        let path_str = path.to_string_lossy();

        // Handle different path formats
        let normalized = if path_str.starts_with("~/") {
            // Handle home directory expansion
            Self::expand_home_path(&path_str)?
        } else if path_str.starts_with("\\\\") {
            // Handle UNC paths - validate but keep as-is
            Self::validate_unc_path(&path_str)?;
            path.clone()
        } else if path_str.len() >= 2 && path_str.chars().nth(1) == Some(':') {
            // Handle Windows drive paths - validate but keep as-is
            Self::validate_drive_path(&path_str)?;
            path.clone()
        } else if path.is_absolute() {
            // Already absolute path
            path.clone()
        } else {
            // Convert relative path to absolute
            std::env::current_dir()
                .map_err(|e| {
                    RezCoreError::BuildError(format!("Cannot get current directory: {}", e))
                })?
                .join(path)
        };

        Ok(normalized)
    }

    /// Expand home directory paths
    fn expand_home_path(path: &str) -> Result<PathBuf, RezCoreError> {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let home_path = PathBuf::from(home);
            Ok(home_path.join(&path[2..]))
        } else {
            Err(RezCoreError::BuildError(
                "Cannot determine home directory".to_string(),
            ))
        }
    }

    /// Validate UNC paths
    fn validate_unc_path(path: &str) -> Result<(), RezCoreError> {
        if !path.starts_with("\\\\") {
            return Err(RezCoreError::BuildError(
                "Invalid UNC path format".to_string(),
            ));
        }

        // Basic UNC path validation: \\server\share\path
        let parts: Vec<&str> = path[2..].split('\\').collect();
        if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(RezCoreError::BuildError(
                "UNC path must be in format \\\\server\\share\\path".to_string(),
            ));
        }

        Ok(())
    }

    /// Validate Windows drive paths
    fn validate_drive_path(path: &str) -> Result<(), RezCoreError> {
        if path.len() < 2 {
            return Err(RezCoreError::BuildError(
                "Invalid drive path format".to_string(),
            ));
        }

        let drive_char = path.chars().nth(0).unwrap();
        if !drive_char.is_ascii_alphabetic() || path.chars().nth(1) != Some(':') {
            return Err(RezCoreError::BuildError(
                "Drive path must start with a letter followed by colon (e.g., C:)".to_string(),
            ));
        }

        Ok(())
    }
}

fn strip_requirement_operator(value: &str) -> &str {
    value
        .strip_prefix(">=")
        .or_else(|| value.strip_prefix("<="))
        .or_else(|| value.strip_prefix("=="))
        .or_else(|| value.strip_prefix("!="))
        .or_else(|| value.strip_prefix('>'))
        .or_else(|| value.strip_prefix('<'))
        .unwrap_or(value)
        .trim()
}

fn canonical_architecture(value: &str) -> String {
    match value.to_ascii_lowercase().as_str() {
        "amd64" | "x64" | "x86-64" | "x86_64" => "x86_64".to_string(),
        "aarch64" | "arm64" | "arm_64" | "armv8" => "arm_64".to_string(),
        "i386" | "i686" => "x86".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rez_next_package::Package;
    use std::path::PathBuf;

    fn make_test_package(name: &str, version: &str) -> Package {
        let mut pkg = Package::new(name.to_string());
        pkg.version = rez_next_version::Version::new(Some(version)).ok();
        pkg
    }

    #[test]
    fn test_standard_env_vars_present() {
        // REZ_BUILD_ENV, REZ_BUILD_TYPE, REZ_BUILD_INSTALL must be set
        let pkg = make_test_package("test-pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        let vars = env.get_env_vars();
        assert_eq!(vars.get("REZ_BUILD_ENV"), Some(&"1".to_string()));
        assert_eq!(vars.get("REZ_BUILD_TYPE"), Some(&"local".to_string()));
        assert_eq!(vars.get("REZ_BUILD_INSTALL"), Some(&"0".to_string()));
    }

    #[test]
    fn test_target_env_vars_follow_host_and_selected_variant() {
        let pkg = make_test_package("test-pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let mut env = BuildEnvironment::new(&pkg, &base, None).unwrap();
        let host = SystemInfo::detect();

        let vars = env.get_env_vars();
        assert_eq!(vars.get("REZ_BUILD_TARGET_ARCH"), Some(&host.arch));
        assert_eq!(vars.get("REZ_BUILD_TARGET_OS"), Some(&host.os));
        assert_eq!(
            vars.get("REZ_BUILD_TARGET_PLATFORM"),
            Some(&format!("{}-{}", host.platform, host.arch))
        );

        env.set_variant_env(
            2,
            &[
                "platform-windows".to_string(),
                "arch-amd64".to_string(),
                "os-windows-10".to_string(),
            ],
        );
        let vars = env.get_env_vars();
        assert_eq!(
            vars.get("REZ_BUILD_TARGET_PLATFORM"),
            Some(&"windows-x86_64".to_string())
        );
        assert_eq!(
            vars.get("REZ_BUILD_TARGET_ARCH"),
            Some(&"x86_64".to_string())
        );
        assert_eq!(
            vars.get("REZ_BUILD_TARGET_OS"),
            Some(&"windows-10".to_string())
        );
    }

    #[test]
    fn test_install_flag_env_var() {
        // When install_path is provided, REZ_BUILD_INSTALL should be "1"
        let pkg = make_test_package("test-pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let install = PathBuf::from("/tmp/install");
        let env = BuildEnvironment::with_install_path(&pkg, &base, None, Some(&install)).unwrap();

        let vars = env.get_env_vars();
        assert_eq!(vars.get("REZ_BUILD_INSTALL"), Some(&"1".to_string()));
    }

    #[test]
    fn test_package_name_version_vars() {
        let pkg = make_test_package("my-package", "2.3.4");
        let base = PathBuf::from("/workspace/build");
        let env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        let vars = env.get_env_vars();
        assert!(!vars.contains_key("REZ_BUILD_PACKAGE_NAME"));
        assert!(!vars.contains_key("REZ_BUILD_PACKAGE_VERSION"));
        assert_eq!(
            vars.get("REZ_BUILD_PROJECT_NAME"),
            Some(&"my-package".to_string())
        );
        assert_eq!(
            vars.get("REZ_BUILD_PROJECT_VERSION"),
            Some(&"2.3.4".to_string())
        );
    }

    #[test]
    fn test_build_type_is_exposed_in_environment() {
        let pkg = make_test_package("my-package", "2.3.4");
        let base = PathBuf::from("/workspace/build");
        let env = BuildEnvironment::with_install_path_and_type(
            &pkg,
            &base,
            None,
            None,
            BuildType::Central,
        )
        .unwrap();

        assert_eq!(
            env.get_env_vars().get("REZ_BUILD_TYPE"),
            Some(&"central".to_string())
        );
    }

    #[test]
    fn test_build_and_install_paths() {
        let pkg = make_test_package("pkg", "1.0.0");
        let base = PathBuf::from("/data/build");
        let env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        let vars = env.get_env_vars();

        // REZ_BUILD_PATH should point to {base}/{package_name}
        let build_path = PathBuf::from(vars.get("REZ_BUILD_PATH").unwrap());
        assert!(
            build_path.ends_with("pkg"),
            "build path should end with 'pkg'"
        );
        assert!(
            build_path
                .parent()
                .map(|p| p.ends_with("build"))
                .unwrap_or(false),
            "build path parent should end with 'build'"
        );

        // REZ_BUILD_INSTALL_PATH should point to {base}/{package_name}/install
        let install_path = PathBuf::from(vars.get("REZ_BUILD_INSTALL_PATH").unwrap());
        assert!(
            install_path.ends_with("install"),
            "install path should end with 'install'"
        );
    }

    #[test]
    fn test_add_and_remove_env_var() {
        let pkg = make_test_package("pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let mut env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        env.add_env_var("CUSTOM_VAR".to_string(), "custom_value".to_string());
        assert_eq!(
            env.get_env_vars().get("CUSTOM_VAR"),
            Some(&"custom_value".to_string())
        );

        env.remove_env_var("CUSTOM_VAR");
        assert!(env.get_env_vars().get("CUSTOM_VAR").is_none());
    }

    #[test]
    fn test_standard_rez_build_vars_present() {
        let mut pkg = make_test_package("pkg", "1.0.0");
        pkg.description = Some(" Test description ".to_string());
        pkg.requires = vec!["python-3.9".to_string()];
        pkg.build_requires = vec!["cmake-3".to_string()];
        pkg.private_build_requires = vec!["internal_lib".to_string()];
        let base = PathBuf::from("/tmp/build");
        let mut env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        env.set_variant_env(1, &["platform-windows".to_string()]);
        env.set_variant_subpath("platform-windows");

        let vars = env.get_env_vars();
        assert_eq!(
            vars.get("REZ_BUILD_PROJECT_DESCRIPTION"),
            Some(&"Test description".to_string())
        );
        assert_eq!(
            vars.get("REZ_BUILD_REQUIRES"),
            Some(&"python-3.9 cmake-3 internal_lib platform-windows".to_string())
        );
        assert_eq!(
            vars.get("REZ_BUILD_REQUIRES_UNVERSIONED"),
            Some(&"python cmake internal_lib platform".to_string())
        );
        assert_eq!(
            vars.get("REZ_BUILD_VARIANT_SUBPATH"),
            Some(&"platform-windows".to_string())
        );
        assert!(
            vars.get("REZ_BUILD_THREAD_COUNT")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default()
                >= 1
        );
    }

    #[test]
    fn test_source_path_env_var() {
        let pkg = make_test_package("pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let source = PathBuf::from("/tmp/source/pkg");
        let mut env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        env.set_source_path(&source);

        assert_eq!(
            env.get_env_vars().get("REZ_BUILD_SOURCE_PATH"),
            Some(&source.to_string_lossy().to_string())
        );
        assert_eq!(
            env.get_env_vars().get("REZ_BUILD_PROJECT_FILE"),
            Some(&source.join("package.py").to_string_lossy().to_string())
        );
    }

    #[test]
    fn test_shell_script_bash() {
        let pkg = make_test_package("pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        let script = env.to_shell_script(&rez_next_context::ShellType::Bash);
        assert!(script.contains("export REZ_BUILD_ENV=\"1\""));
        assert!(script.contains("#!/bin/bash"));
    }

    #[test]
    fn test_shell_script_powershell() {
        let pkg = make_test_package("pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        let script = env.to_shell_script(&rez_next_context::ShellType::PowerShell);
        assert!(script.contains("$env:REZ_BUILD_ENV = '1'"));
    }

    #[test]
    fn test_normalize_build_path_absolute() {
        // Use a path that is absolute on both Unix and Windows
        // On Windows, "C:\..." is absolute; on Unix, "/..." is absolute
        #[cfg(unix)]
        let path = PathBuf::from("/absolute/path/to/build");
        #[cfg(windows)]
        let path = PathBuf::from("C:\\absolute\\path\\to\\build");

        let normalized = BuildEnvironment::normalize_build_path(&path).unwrap();
        assert_eq!(normalized, path);
    }

    #[test]
    fn test_normalize_build_path_relative() {
        // Relative path should be converted to absolute
        let path = PathBuf::from("relative/build");
        let result = BuildEnvironment::normalize_build_path(&path);
        assert!(result.is_ok());
        let normalized = result.unwrap();
        assert!(normalized.is_absolute());
    }

    #[test]
    fn test_get_dirs() {
        let pkg = make_test_package("pkg", "1.0.0");
        let base = PathBuf::from("/tmp/build");
        let env = BuildEnvironment::new(&pkg, &base, None).unwrap();

        assert!(env.get_build_dir().ends_with("pkg"));
        assert!(env.get_install_dir().ends_with("install"));
        assert!(env.get_temp_dir().ends_with("tmp"));
    }
}
