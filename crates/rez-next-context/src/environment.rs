//! Environment variable generation and management

use crate::{ContextConfig, PathStrategy};
use regex::{Captures, Regex};
use rez_next_common::RezCoreError;
use rez_next_package::{Package, deferred_commands::materialize_deferred_commands};
use rez_next_rex::{RexAction, RexActionType, RexEnvironment, RexExecutor};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::Path;
use std::sync::OnceLock;

/// Environment manager for generating package environments
///
/// Note: PyO3 `#[pyclass]` is disabled until DLL layout allows safe cross-crate
/// type sharing. See crates/rez-next-python/src/environment_bindings.rs for
/// the Python-facing wrapper.
#[derive(Debug, Clone)]
pub struct EnvironmentManager {
    /// Configuration for environment generation
    config: ContextConfig,
    /// Parent environment available to Rez inheritance rules.
    parent_env: HashMap<String, String>,
    /// Environment exported before package Rex actions are applied.
    base_env: HashMap<String, String>,
}

/// Environment variable operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnvOperation {
    /// Set a variable to a value
    Set(String),
    /// Prepend to a variable (with separator)
    Prepend(String, String), // value, separator
    /// Append to a variable (with separator)
    Append(String, String), // value, separator
    /// Unset a variable
    Unset,
    /// Set a variable only when it is not already defined
    SetIfEmpty(String),
    /// Restore a variable to its original parent value
    Reset,
}

/// Environment variable definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVarDefinition {
    /// Variable name
    pub name: String,
    /// Operation to perform
    pub operation: EnvOperation,
    /// Source package (if any)
    pub source_package: Option<String>,
    /// Priority (higher = applied later)
    pub priority: i32,
}

impl EnvironmentManager {
    /// Create a new environment manager
    pub fn new(config: ContextConfig) -> Self {
        let parent_env = env::vars()
            .map(|(name, value)| (environment_key(&name), value))
            .collect::<HashMap<_, _>>();
        let base_env = if config.inherit_parent_env {
            parent_env.clone()
        } else {
            HashMap::new()
        };

        Self {
            config,
            parent_env,
            base_env,
        }
    }

    /// Create a manager with an explicit base environment.
    ///
    /// Package Rex commands are applied after this environment, unlike
    /// `ContextConfig::additional_env_vars`, which intentionally applies last.
    pub fn with_base_environment(
        config: ContextConfig,
        environment: HashMap<String, String>,
    ) -> Self {
        let parent_env = Self::normalize_parent_environment(environment);
        let base_env = parent_env.clone();
        Self {
            config,
            parent_env,
            base_env,
        }
    }

    /// Create a manager with an explicit parent environment.
    ///
    /// The parent is always visible to Rex expressions and reset operations.
    /// It is exported only through the configured Rez inheritance policy.
    pub fn with_parent_environment(
        config: ContextConfig,
        environment: HashMap<String, String>,
    ) -> Self {
        let parent_env = Self::normalize_parent_environment(environment);
        let base_env = if config.inherit_parent_env {
            parent_env.clone()
        } else {
            HashMap::new()
        };
        Self {
            config,
            parent_env,
            base_env,
        }
    }

    fn normalize_parent_environment(
        environment: HashMap<String, String>,
    ) -> HashMap<String, String> {
        let mut entries: Vec<_> = environment.into_iter().collect();
        if cfg!(windows) {
            entries.sort_by(|(left, _), (right, _)| {
                let left_key = left.to_ascii_uppercase();
                let right_key = right.to_ascii_uppercase();
                left_key.cmp(&right_key).then_with(|| {
                    let left_is_canonical = left == &left_key;
                    let right_is_canonical = right == &right_key;
                    left_is_canonical.cmp(&right_is_canonical)
                })
            });
        }
        let mut parent_env = HashMap::new();
        for (name, value) in entries {
            parent_env.insert(environment_key(&name), value);
        }
        parent_env
    }

    /// Generate environment variables for a list of packages
    pub async fn generate_environment(
        &self,
        packages: &[Package],
    ) -> Result<HashMap<String, String>, RezCoreError> {
        Ok(self.generate_rex_environment(packages).await?.vars)
    }

    /// Generate the complete Rex activation state, including aliases and startup actions.
    pub async fn generate_rex_environment(
        &self,
        packages: &[Package],
    ) -> Result<RexEnvironment, RezCoreError> {
        self.generate_rex_environment_with_ephemerals(packages, &HashSet::new())
            .await
    }

    pub(crate) async fn generate_rex_environment_with_ephemerals(
        &self,
        packages: &[Package],
        resolved_ephemeral_names: &HashSet<String>,
    ) -> Result<RexEnvironment, RezCoreError> {
        let mut env_vars = self.base_env.clone();
        let mut env_definitions = Vec::new();
        let mut shell_actions = Vec::new();
        let mut priority = 0;
        let resolved_package_names = packages
            .iter()
            .map(|package| package.name.clone())
            .collect::<HashSet<_>>();
        let building = self
            .config
            .additional_env_vars
            .keys()
            .any(|name| name.eq_ignore_ascii_case("REZ_BUILD_ENV"));

        // Package metadata is available to every Rex phase.
        for package in packages {
            env_definitions.extend(self.package_metadata_definitions(package, priority));
            priority += 1;
        }

        // Rez runs each command phase across the whole resolve before advancing
        // to the next phase. In particular, pre_commands must finish before any
        // package commands add paths back to the environment.
        type CommandPhase = for<'a> fn(&'a Package) -> Option<&'a str>;
        let command_phases: [CommandPhase; 3] = [
            |package| package.pre_commands.as_deref(),
            |package| package.commands.as_deref(),
            |package| package.post_commands.as_deref(),
        ];
        for commands_for in command_phases {
            for package in packages {
                if let Some(commands) = commands_for(package) {
                    let (definitions, actions) = self.rex_effects(
                        package,
                        commands,
                        priority,
                        &resolved_package_names,
                        resolved_ephemeral_names,
                        building,
                    )?;
                    env_definitions.extend(definitions);
                    shell_actions.extend(actions);
                }
                priority += 1;
            }
        }

        // Add additional environment variables from config
        for (name, value) in &self.config.additional_env_vars {
            env_definitions.push(EnvVarDefinition {
                name: name.clone(),
                operation: EnvOperation::Set(value.clone()),
                source_package: None,
                priority,
            });
        }

        // Sort by priority (lower priority first)
        env_definitions.sort_by_key(|def| def.priority);

        // Apply environment variable definitions
        for env_def in env_definitions {
            self.apply_env_definition(&mut env_vars, &env_def)?;
        }

        // Handle PATH modifications
        self.apply_path_modifications(&mut env_vars, packages)?;

        // Unset specified variables
        for var_name in &self.config.unset_vars {
            env_vars.remove(&environment_key(var_name));
        }

        normalize_environment_paths(&mut env_vars);

        let mut environment = RexEnvironment::new();
        environment.vars = env_vars;
        environment.apply(&shell_actions);
        Ok(environment)
    }

    fn package_metadata_definitions(
        &self,
        package: &Package,
        priority: i32,
    ) -> Vec<EnvVarDefinition> {
        let mut definitions = Vec::new();

        // Always set package root and version variables. Real repository packages
        // carry their descriptor path; synthetic packages keep the legacy fallback.
        let package_root = Self::package_root(package);
        let legacy_prefix = package.name.to_uppercase();
        let rez_prefix = format!("REZ_{}", legacy_prefix.replace('.', "_"));
        definitions.push(EnvVarDefinition {
            name: format!("{legacy_prefix}_ROOT"),
            operation: EnvOperation::Set(package_root.clone()),
            source_package: Some(package.name.clone()),
            priority,
        });
        definitions.push(EnvVarDefinition {
            name: format!("{rez_prefix}_ROOT"),
            operation: EnvOperation::Set(package_root.clone()),
            source_package: Some(package.name.clone()),
            priority,
        });
        definitions.push(EnvVarDefinition {
            name: format!("{rez_prefix}_BASE"),
            operation: EnvOperation::Set(
                package.base.clone().unwrap_or_else(|| package_root.clone()),
            ),
            source_package: Some(package.name.clone()),
            priority,
        });

        let version_string = package
            .version
            .as_ref()
            .map_or_else(String::new, |version| version.as_str().to_string());
        definitions.push(EnvVarDefinition {
            name: format!("{legacy_prefix}_VERSION"),
            operation: EnvOperation::Set(version_string.clone()),
            source_package: Some(package.name.clone()),
            priority,
        });
        definitions.push(EnvVarDefinition {
            name: format!("{rez_prefix}_VERSION"),
            operation: EnvOperation::Set(version_string),
            source_package: Some(package.name.clone()),
            priority,
        });
        for (index, component) in ["MAJOR", "MINOR", "PATCH"].into_iter().enumerate() {
            definitions.push(EnvVarDefinition {
                name: format!("{rez_prefix}_{component}_VERSION"),
                operation: EnvOperation::Set(
                    package
                        .version
                        .as_ref()
                        .and_then(|version| match index {
                            0 => version.major(),
                            1 => version.minor(),
                            2 => version.patch(),
                            _ => None,
                        })
                        .map_or_else(String::new, |component| component.to_string()),
                ),
                source_package: Some(package.name.clone()),
                priority,
            });
        }

        definitions
    }

    /// Convert one package command phase into environment definitions.
    fn rex_effects(
        &self,
        package: &Package,
        commands: &str,
        priority: i32,
        resolved_package_names: &HashSet<String>,
        resolved_ephemeral_names: &HashSet<String>,
        building: bool,
    ) -> Result<(Vec<EnvVarDefinition>, Vec<RexAction>), RezCoreError> {
        let mut definitions = Vec::new();
        let mut shell_actions = Vec::new();
        let package_root = Self::package_root(package);
        let mut executor = RexExecutor::new();

        // Set context variables for this package
        executor.set_context_var("root", &package_root);
        if let Some(ref version) = package.version {
            executor.set_context_var("version", version.as_str());
        }
        executor.set_context_var("name", &package.name);

        let commands = materialize_deferred_commands(
            commands,
            resolved_package_names,
            resolved_ephemeral_names,
            building,
        )?;
        executor.execute_commands(
            &commands,
            &package.name,
            Some(&package_root),
            package.version.as_ref().map(|v| v.as_str()),
        )?;
        for action in executor.get_actions() {
            let definition = match &action.action_type {
                RexActionType::Setenv { name, value } => {
                    Some((name.clone(), EnvOperation::Set(value.clone())))
                }
                RexActionType::Unsetenv { name } => Some((name.clone(), EnvOperation::Unset)),
                RexActionType::PrependPath {
                    name,
                    value,
                    separator,
                } => Some((
                    name.clone(),
                    EnvOperation::Prepend(
                        value.clone(),
                        separator.clone().unwrap_or_else(get_path_separator),
                    ),
                )),
                RexActionType::AppendPath {
                    name,
                    value,
                    separator,
                } => Some((
                    name.clone(),
                    EnvOperation::Append(
                        value.clone(),
                        separator.clone().unwrap_or_else(get_path_separator),
                    ),
                )),
                RexActionType::SetenvIfEmpty { name, value } => {
                    Some((name.clone(), EnvOperation::SetIfEmpty(value.clone())))
                }
                RexActionType::Resetenv { name } => Some((name.clone(), EnvOperation::Reset)),
                _ => None,
            };
            let Some((name, operation)) = definition else {
                shell_actions.push(action.clone());
                continue;
            };
            definitions.push(EnvVarDefinition {
                name,
                operation,
                source_package: Some(package.name.clone()),
                priority,
            });
        }

        Ok((definitions, shell_actions))
    }

    fn package_root(package: &Package) -> String {
        package
            .root()
            .unwrap_or_else(|| format!("/packages/{}", package.name))
    }

    fn join_root_path(root: &str, child: &str) -> String {
        Path::new(root).join(child).to_string_lossy().to_string()
    }

    fn package_tool_path(package: &Package) -> String {
        let root = Self::package_root(package);
        let root_path = Path::new(&root);
        let has_root_tool = package.tools.iter().any(|tool| {
            ["", ".exe", ".bat", ".cmd", ".com"]
                .iter()
                .any(|extension| root_path.join(format!("{}{}", tool, extension)).is_file())
        });
        if has_root_tool {
            root
        } else {
            Self::join_root_path(&root, "bin")
        }
    }

    /// Apply an environment variable definition
    fn apply_env_definition(
        &self,
        env_vars: &mut HashMap<String, String>,
        env_def: &EnvVarDefinition,
    ) -> Result<(), RezCoreError> {
        let name = environment_key(&env_def.name);
        match &env_def.operation {
            EnvOperation::Set(value) => {
                let value = self.expand_environment_value(value, env_vars);
                env_vars.insert(name, value);
            }
            EnvOperation::Prepend(value, separator) => {
                let value = self.expand_environment_value(value, env_vars);
                let current = self.current_or_selected_parent_value(env_vars, &name);
                let new_value = if current.is_empty() {
                    value
                } else {
                    format!("{}{}{}", value, separator, current)
                };
                env_vars.insert(name, new_value);
            }
            EnvOperation::Append(value, separator) => {
                let value = self.expand_environment_value(value, env_vars);
                let current = self.current_or_selected_parent_value(env_vars, &name);
                let new_value = if current.is_empty() {
                    value
                } else {
                    format!("{}{}{}", current, separator, value)
                };
                env_vars.insert(name, new_value);
            }
            EnvOperation::Unset => {
                env_vars.remove(&name);
            }
            EnvOperation::SetIfEmpty(value) => {
                if !env_vars.contains_key(&name) && !self.parent_env.contains_key(&name) {
                    let value = self.expand_environment_value(value, env_vars);
                    env_vars.insert(name, value);
                }
            }
            EnvOperation::Reset => match self.parent_env.get(&name) {
                Some(value) => {
                    env_vars.insert(name, value.clone());
                }
                None => {
                    env_vars.remove(&name);
                }
            },
        }

        Ok(())
    }

    fn expand_environment_value(
        &self,
        value: &str,
        environment: &HashMap<String, String>,
    ) -> String {
        static ENVIRONMENT_REFERENCE: OnceLock<Regex> = OnceLock::new();
        let expression = ENVIRONMENT_REFERENCE.get_or_init(|| {
            Regex::new(r"\$(?:\{(?P<braced>[^}]*)\}|(?P<plain>\w+))")
                .expect("environment reference regex is valid")
        });
        expression
            .replace_all(value, |captures: &Captures<'_>| {
                let name = captures
                    .name("braced")
                    .or_else(|| captures.name("plain"))
                    .expect("one environment reference group matches")
                    .as_str();
                let key = environment_key(name);
                environment
                    .get(&key)
                    .or_else(|| self.parent_env.get(&key))
                    .cloned()
                    .unwrap_or_else(|| captures[0].to_string())
            })
            .into_owned()
    }

    fn current_or_selected_parent_value(
        &self,
        environment: &HashMap<String, String>,
        name: &str,
    ) -> String {
        environment.get(name).cloned().unwrap_or_else(|| {
            let inherits_parent = self.config.all_parent_variables
                || self
                    .config
                    .parent_variables
                    .iter()
                    .any(|variable| environment_key(variable) == name);
            if inherits_parent {
                self.parent_env.get(name).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        })
    }

    /// Apply PATH modifications based on package tools
    fn apply_path_modifications(
        &self,
        env_vars: &mut HashMap<String, String>,
        packages: &[Package],
    ) -> Result<(), RezCoreError> {
        if self.config.path_strategy == PathStrategy::NoModify {
            return Ok(());
        }

        let mut tool_paths = Vec::new();

        // Collect tool paths from packages
        for package in packages {
            for _tool in &package.tools {
                let tool_path = Self::package_tool_path(package);
                if !tool_paths.contains(&tool_path) {
                    tool_paths.push(tool_path);
                }
            }
        }

        if tool_paths.is_empty() {
            return Ok(());
        }

        let path_separator = get_path_separator();
        let new_path_segment = tool_paths.join(&path_separator);
        let path_key = environment_key("PATH");
        let current_path = env_vars.get(&path_key).cloned().unwrap_or_default();

        let new_path = match self.config.path_strategy {
            PathStrategy::Prepend => {
                if current_path.is_empty() {
                    new_path_segment
                } else {
                    format!("{}{}{}", new_path_segment, path_separator, current_path)
                }
            }
            PathStrategy::Append => {
                if current_path.is_empty() {
                    new_path_segment
                } else {
                    format!("{}{}{}", current_path, path_separator, new_path_segment)
                }
            }
            PathStrategy::Replace => new_path_segment,
            PathStrategy::NoModify => current_path,
        };

        env_vars.insert(path_key, new_path);
        Ok(())
    }

    /// Generate shell script for environment setup
    pub fn generate_shell_script(
        &self,
        env_vars: &HashMap<String, String>,
    ) -> Result<String, RezCoreError> {
        let environment = RexEnvironment {
            vars: env_vars.clone(),
            ..Default::default()
        };
        Ok(rez_next_rex::generate_shell_script(
            &environment,
            &self.config.shell_type,
        ))
    }

    /// Get environment variable differences from base environment
    pub fn get_env_diff(&self, env_vars: &HashMap<String, String>) -> EnvDiff {
        let mut added = HashMap::new();
        let mut modified = HashMap::new();
        let mut removed = Vec::new();

        // Find added and modified variables
        for (name, value) in env_vars {
            match self.base_env.get(name) {
                Some(base_value) => {
                    if base_value != value {
                        modified.insert(name.clone(), (base_value.clone(), value.clone()));
                    }
                }
                None => {
                    added.insert(name.clone(), value.clone());
                }
            }
        }

        // Find removed variables
        for name in self.base_env.keys() {
            if !env_vars.contains_key(name) {
                removed.push(name.clone());
            }
        }

        EnvDiff {
            added,
            modified,
            removed,
        }
    }
}

/// Environment variable differences
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvDiff {
    /// Added variables
    pub added: HashMap<String, String>,
    /// Modified variables (old_value, new_value)
    pub modified: HashMap<String, (String, String)>,
    /// Removed variables
    pub removed: Vec<String>,
}

impl EnvDiff {
    /// Check if there are any differences
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }

    /// Get the total number of changes
    pub fn change_count(&self) -> usize {
        self.added.len() + self.modified.len() + self.removed.len()
    }
}

/// Get platform-appropriate path separator
fn get_path_separator() -> String {
    if cfg!(windows) {
        ";".to_string()
    } else {
        ":".to_string()
    }
}

fn environment_key(name: &str) -> String {
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

/// Remove duplicate PATH entries while preserving their activation order.
///
/// This also keeps Windows command environments below `cmd.exe`'s practical
/// PATH-length limit when nested tool managers repeat their own search paths.
pub fn normalize_environment_paths(env_vars: &mut HashMap<String, String>) {
    let path_key = env_vars
        .keys()
        .find(|name| name.eq_ignore_ascii_case("PATH"))
        .cloned()
        .unwrap_or_else(|| environment_key("PATH"));
    let Some(path) = env_vars.get_mut(&path_key) else {
        return;
    };
    let separator = get_path_separator();
    let mut seen = HashSet::new();
    let entries = path
        .split(&separator)
        .filter(|entry| {
            let identity = if cfg!(windows) {
                entry.replace('/', "\\").to_ascii_lowercase()
            } else {
                (*entry).to_string()
            };
            seen.insert(identity)
        })
        .collect::<Vec<_>>();
    *path = entries.join(&separator);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextConfig;
    use std::path::PathBuf;

    #[test]
    fn test_env_operation_variants() {
        let _ = EnvOperation::Set("value".to_string());
        let _ = EnvOperation::Prepend("value".to_string(), ":".to_string());
        let _ = EnvOperation::Append("value".to_string(), ";".to_string());
        let _ = EnvOperation::Unset;
    }

    #[test]
    fn test_env_var_definition_creation() {
        let def = EnvVarDefinition {
            name: "TEST_VAR".to_string(),
            operation: EnvOperation::Set("test_value".to_string()),
            source_package: Some("test_pkg".to_string()),
            priority: 50,
        };

        assert_eq!(def.name, "TEST_VAR");
        assert_eq!(def.priority, 50);
        assert_eq!(def.source_package, Some("test_pkg".to_string()));
    }

    #[test]
    fn test_env_var_definition_no_source() {
        let def = EnvVarDefinition {
            name: "GLOBAL_VAR".to_string(),
            operation: EnvOperation::Set("global_value".to_string()),
            source_package: None,
            priority: 1000,
        };

        assert!(def.source_package.is_none());
        assert_eq!(def.priority, 1000);
    }

    #[test]
    fn test_env_diff_is_empty_true() {
        let diff = EnvDiff {
            added: HashMap::new(),
            modified: HashMap::new(),
            removed: Vec::new(),
        };

        assert!(diff.is_empty());
        assert_eq!(diff.change_count(), 0);
    }

    #[test]
    fn test_env_diff_is_empty_false_with_added() {
        let mut added = HashMap::new();
        added.insert("NEW_VAR".to_string(), "value".to_string());

        let diff = EnvDiff {
            added,
            modified: HashMap::new(),
            removed: Vec::new(),
        };

        assert!(!diff.is_empty());
        assert_eq!(diff.change_count(), 1);
    }

    #[test]
    fn test_env_diff_is_empty_false_with_modified() {
        let mut modified = HashMap::new();
        modified.insert(
            "MOD_VAR".to_string(),
            ("old".to_string(), "new".to_string()),
        );

        let diff = EnvDiff {
            added: HashMap::new(),
            modified,
            removed: Vec::new(),
        };

        assert!(!diff.is_empty());
        assert_eq!(diff.change_count(), 1);
    }

    #[test]
    fn test_env_diff_is_empty_false_with_removed() {
        let diff = EnvDiff {
            added: HashMap::new(),
            modified: HashMap::new(),
            removed: vec!["OLD_VAR".to_string()],
        };

        assert!(!diff.is_empty());
        assert_eq!(diff.change_count(), 1);
    }

    #[test]
    fn test_env_diff_change_count_multiple() {
        let mut added = HashMap::new();
        added.insert("VAR1".to_string(), "val1".to_string());
        added.insert("VAR2".to_string(), "val2".to_string());

        let mut modified = HashMap::new();
        modified.insert("VAR3".to_string(), ("old".to_string(), "new".to_string()));

        let diff = EnvDiff {
            added,
            modified,
            removed: vec!["VAR4".to_string(), "VAR5".to_string()],
        };

        assert_eq!(diff.change_count(), 5);
    }

    #[test]
    fn test_get_path_separator() {
        let separator = get_path_separator();
        // On Windows, should be ";", on Unix ":"
        if cfg!(windows) {
            assert_eq!(separator, ";");
        } else {
            assert_eq!(separator, ":");
        }
    }

    #[test]
    fn test_environment_manager_new_with_inherit() {
        let config = ContextConfig {
            inherit_parent_env: true,
            ..Default::default()
        };
        let manager = EnvironmentManager::new(config);

        // base_env should have some variables (since we inherit)
        // Note: this test may behave differently in different environments
        // Just verify the manager was created successfully
        let _ = manager;
    }

    #[test]
    fn test_environment_manager_new_without_inherit() {
        let config = ContextConfig {
            inherit_parent_env: false,
            ..Default::default()
        };
        let manager = EnvironmentManager::new(config);

        // base_env should be empty
        // Note: we can't directly access base_env since it's private
        // This test just verifies creation doesn't panic
        let _ = manager;
    }

    #[test]
    fn test_package_tool_at_variant_root_is_added_to_path() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let temp = tempfile::TempDir::new().unwrap();
        let tool = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        std::fs::write(temp.path().join(tool), b"").unwrap();
        let package = Package {
            name: "python".to_string(),
            tools: vec!["python".to_string()],
            filepath: Some(
                temp.path()
                    .join("package.py")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        };
        let manager = EnvironmentManager::new(ContextConfig {
            inherit_parent_env: false,
            ..Default::default()
        });

        let environment = rt
            .block_on(manager.generate_environment(&[package]))
            .unwrap();
        assert_eq!(
            environment.get("PATH"),
            Some(&temp.path().to_string_lossy().into_owned())
        );
    }

    #[test]
    fn test_package_path_actions_accumulate_and_expand_this_root() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let temp = tempfile::TempDir::new().unwrap();
        let packages: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|name| {
                let root = temp.path().join(name);
                std::fs::create_dir_all(&root).unwrap();
                Package {
                    name: name.to_string(),
                    filepath: Some(root.join("package.py").to_string_lossy().into_owned()),
                    commands: Some(
                        "env.prepend_path('PYTHONPATH', '{this.root}/site-packages')".to_string(),
                    ),
                    ..Default::default()
                }
            })
            .collect();
        let manager = EnvironmentManager::new(ContextConfig {
            inherit_parent_env: false,
            ..Default::default()
        });

        let environment = rt
            .block_on(manager.generate_environment(&packages))
            .unwrap();
        let python_path = environment.get("PYTHONPATH").unwrap();
        assert!(python_path.contains("first"));
        assert!(python_path.contains("second"));
        assert!(!python_path.contains("{this.root}"));
    }

    #[test]
    fn test_package_environment_uses_one_path_key_and_runs_pre_commands() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let temp = tempfile::TempDir::new().unwrap();
        let first = Package {
            name: "first".to_string(),
            filepath: Some(
                temp.path()
                    .join("package.py")
                    .to_string_lossy()
                    .into_owned(),
            ),
            pre_commands: Some("unsetenv('PYTHONPATH')".to_string()),
            commands: Some("env.prepend_path('PATH', '{this.root}/bin')".to_string()),
            ..Default::default()
        };
        let second_root = temp.path().join("second");
        let second = Package {
            name: "second".to_string(),
            filepath: Some(
                second_root
                    .join("package.py")
                    .to_string_lossy()
                    .into_owned(),
            ),
            commands: Some(
                "env.prepend_path('PYTHONPATH', '{this.root}/site-packages')".to_string(),
            ),
            ..Default::default()
        };
        let manager = EnvironmentManager::new(ContextConfig {
            inherit_parent_env: true,
            ..Default::default()
        });

        let environment = rt
            .block_on(manager.generate_environment(&[first, second]))
            .unwrap();
        let path_keys: Vec<_> = environment
            .keys()
            .filter(|name| name.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(path_keys.len(), 1);
        let path: Vec<_> =
            std::env::split_paths(environment.get(&environment_key("PATH")).unwrap()).collect();
        assert_eq!(path.first(), Some(&temp.path().join("bin")));
        let python_path: Vec<_> =
            std::env::split_paths(environment.get(&environment_key("PYTHONPATH")).unwrap())
                .collect();
        assert_eq!(
            python_path.first(),
            Some(&second_root.join("site-packages"))
        );
    }

    #[test]
    fn test_explicit_base_environment_precedes_package_commands() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let temp = tempfile::TempDir::new().unwrap();
        let package = Package {
            name: "tool".to_string(),
            filepath: Some(
                temp.path()
                    .join("package.py")
                    .to_string_lossy()
                    .into_owned(),
            ),
            commands: Some("env.prepend_path('PATH', '{root}/bin')".to_string()),
            tools: vec!["tool".to_string()],
            ..Default::default()
        };
        let separator = get_path_separator();
        let mut base =
            HashMap::from([("PATH".to_string(), format!("base-path{separator}base-path"))]);
        if cfg!(windows) {
            base.insert(
                "Path".to_string(),
                format!("shadow-path{separator}shadow-path"),
            );
        }
        let manager = EnvironmentManager::with_base_environment(
            ContextConfig {
                inherit_parent_env: false,
                ..Default::default()
            },
            base,
        );

        let environment = rt
            .block_on(manager.generate_environment(&[package]))
            .unwrap();
        let path: Vec<_> =
            std::env::split_paths(environment.get(&environment_key("PATH")).unwrap()).collect();
        let normalized_root = temp.path().join("bin");
        assert_eq!(path.first(), Some(&normalized_root));
        let base_entry = "base-path";
        let base_path = PathBuf::from(base_entry);
        assert_eq!(path.last(), Some(&base_path));
        assert_eq!(
            path.iter()
                .filter(|entry| *entry == &normalized_root)
                .count(),
            1
        );
        assert_eq!(path.iter().filter(|entry| *entry == &base_path).count(), 1);
    }
}
