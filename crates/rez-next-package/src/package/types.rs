//! Package type definition.

use rez_next_version::Version;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use super::requirement::PackageRequirement;

fn default_test_run_on() -> Vec<String> {
    vec!["default".to_string()]
}

fn deserialize_test_run_on<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawRunOn {
        String(String),
        List(Vec<String>),
    }

    match RawRunOn::deserialize(deserializer)? {
        RawRunOn::String(tag) => Ok(vec![tag]),
        RawRunOn::List(tags) => Ok(tags),
    }
}

/// Command representation used by a package test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestCommand {
    /// Execute a shell command string.
    String(String),
    /// Execute an argv command without a shell.
    List(Vec<String>),
}

impl TestCommand {
    /// Render the command for display and legacy callers.
    pub fn as_string(&self) -> String {
        match self {
            Self::String(command) => command.clone(),
            Self::List(arguments) => arguments.join(" "),
        }
    }

    /// Expand package-root tokens in a test command.
    ///
    /// Rez formats both shell commands and argv commands with the selected
    /// variant before execution. The build and test entry points share this
    /// method so that package tests behave consistently in every workflow.
    #[must_use]
    pub fn expanded_for_root(&self, root: &Path) -> Self {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root = root.to_string_lossy();
        let expand = |value: &str| {
            value
                .replace("{this.root}", &root)
                .replace("{this.base}", &root)
                .replace("{root}", &root)
                .replace("{base}", &root)
        };

        match self {
            Self::String(command) => Self::String(expand(command)),
            Self::List(arguments) => {
                Self::List(arguments.iter().map(|argument| expand(argument)).collect())
            }
        }
    }

    /// Append command-line arguments using the execution mode of the command.
    ///
    /// String commands are interpreted by a shell and therefore receive
    /// shell-quoted arguments. List commands are launched directly and retain
    /// each argument as one argv item.
    #[must_use]
    pub fn with_extra_args(&self, extra_args: &[String]) -> Self {
        if extra_args.is_empty() {
            return self.clone();
        }

        match self {
            Self::String(command) => Self::String(format!(
                "{command} {}",
                extra_args
                    .iter()
                    .map(|argument| shell_quote(argument))
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
            Self::List(arguments) => Self::List(
                arguments
                    .iter()
                    .cloned()
                    .chain(extra_args.iter().cloned())
                    .collect(),
            ),
        }
    }
}

/// Create a process configured to execute a shell command string.
pub fn shell_command(command: &str) -> Command {
    let mut process = if cfg!(windows) {
        Command::new("cmd")
    } else {
        Command::new("sh")
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        process.raw_arg(format!("/d /s /c {command}"));
    }

    #[cfg(not(windows))]
    {
        process.args(["-c", command]);
    }

    process
}

fn shell_quote(argument: &str) -> String {
    #[cfg(windows)]
    {
        if argument.is_empty()
            || argument.chars().any(|character| {
                character.is_whitespace() || matches!(character, '"' | '&' | '|' | '<' | '>')
            })
        {
            return format!("\"{}\"", argument.replace('"', "\\\""));
        }
        argument.to_string()
    }

    #[cfg(not(windows))]
    {
        if !argument.is_empty()
            && argument.chars().all(|character| {
                character.is_ascii_alphanumeric() || "_+-=.,/@:%".contains(character)
            })
        {
            return argument.to_string();
        }
        format!("'{}'", argument.replace('\'', "'\\''"))
    }
}

impl Serialize for TestCommand {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::String(command) => serializer.serialize_str(command),
            Self::List(arguments) => arguments.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for TestCommand {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawCommand {
            String(String),
            List(Vec<String>),
        }

        match RawCommand::deserialize(deserializer)? {
            RawCommand::String(command) => Ok(Self::String(command)),
            RawCommand::List(arguments) => Ok(Self::List(arguments)),
        }
    }
}

/// Variant selection rule for a package test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestVariantFilter {
    /// Run once without requiring a variant-specific match.
    Enabled(bool),
    /// Run on variants whose requirements include all listed requirements.
    Requires(Vec<String>),
}

impl TestVariantFilter {
    /// Check whether a variant's direct requirements satisfy this filter.
    ///
    /// Rez variant filters use requirement strings such as `python-3.11` and
    /// `!maya`. The filter is evaluated against the direct requirements of the
    /// selected variant, before the test environment is resolved.
    pub fn matches_variant_requires(&self, variant_requires: &[String]) -> bool {
        match self {
            Self::Enabled(enabled) => *enabled,
            Self::Requires(filters) => {
                let variant_requirements: Vec<_> = variant_requires
                    .iter()
                    .filter_map(|requirement| PackageRequirement::parse(requirement).ok())
                    .collect();

                filters.iter().all(|filter| {
                    let Ok(filter) = PackageRequirement::parse(filter) else {
                        return false;
                    };
                    if filter.conflict {
                        !variant_requirements.iter().any(|variant| {
                            !variant.conflict && requirements_match(&filter, variant)
                        })
                    } else {
                        variant_requirements.iter().any(|variant| {
                            !variant.conflict && requirements_match(&filter, variant)
                        })
                    }
                })
            }
        }
    }
}

fn requirements_match(filter: &PackageRequirement, variant: &PackageRequirement) -> bool {
    if filter.name != variant.name {
        return false;
    }

    match filter.version_spec() {
        None => true,
        Some(filter_version) => variant.version_spec().is_some_and(|variant_version| {
            if filter_version == variant_version {
                return true;
            }
            let Ok(version) = Version::parse(variant_version) else {
                return false;
            };
            filter.satisfied_by(&version)
        }),
    }
}

impl Serialize for TestVariantFilter {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Enabled(enabled) => serializer.serialize_bool(*enabled),
            Self::Requires(requirements) => {
                use serde::ser::SerializeStruct;
                let mut state = serializer.serialize_struct("TestVariantFilter", 2)?;
                state.serialize_field("type", "requires")?;
                state.serialize_field("value", requirements)?;
                state.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for TestVariantFilter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawFilter {
            Enabled(bool),
            Requires {
                #[serde(rename = "type")]
                kind: String,
                value: Vec<String>,
            },
        }

        match RawFilter::deserialize(deserializer)? {
            RawFilter::Enabled(enabled) => Ok(Self::Enabled(enabled)),
            RawFilter::Requires { kind, value } if kind == "requires" => Ok(Self::Requires(value)),
            RawFilter::Requires { kind, .. } => Err(de::Error::custom(format!(
                "unsupported test on_variants type '{kind}'"
            ))),
        }
    }
}

/// Rez-compatible definition for one package test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageTestDefinition {
    /// Command to execute.
    pub command: TestCommand,
    /// Additional packages required by this test.
    pub requires: Vec<String>,
    /// Tags on which the test is selected.
    pub run_on: Vec<String>,
    /// Variant selection rule.
    pub on_variants: Option<TestVariantFilter>,
}

impl PackageTestDefinition {
    /// Build a legacy string test definition with Rez defaults.
    pub fn from_command(command: TestCommand) -> Self {
        Self {
            command,
            requires: Vec::new(),
            run_on: default_test_run_on(),
            on_variants: None,
        }
    }
}

impl Serialize for PackageTestDefinition {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let is_simple = self.requires.is_empty()
            && self.run_on == default_test_run_on()
            && self.on_variants.is_none();
        if is_simple {
            return self.command.serialize(serializer);
        }

        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("PackageTestDefinition", 4)?;
        state.serialize_field("command", &self.command)?;
        state.serialize_field("requires", &self.requires)?;
        state.serialize_field("run_on", &self.run_on)?;
        state.serialize_field("on_variants", &self.on_variants)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for PackageTestDefinition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawDefinition {
            String(String),
            List(Vec<String>),
            Structured {
                command: TestCommand,
                #[serde(default)]
                requires: Vec<String>,
                #[serde(
                    default = "default_test_run_on",
                    deserialize_with = "deserialize_test_run_on"
                )]
                run_on: Vec<String>,
                #[serde(default)]
                on_variants: Option<TestVariantFilter>,
            },
        }

        match RawDefinition::deserialize(deserializer)? {
            RawDefinition::String(command) => Ok(Self::from_command(TestCommand::String(command))),
            RawDefinition::List(arguments) => Ok(Self::from_command(TestCommand::List(arguments))),
            RawDefinition::Structured {
                command,
                requires,
                run_on,
                on_variants,
            } => Ok(Self {
                command,
                requires,
                run_on,
                on_variants,
            }),
        }
    }
}

/// High-performance package representation compatible with rez
#[derive(Debug, Clone, Default)]
pub struct Package {
    /// Package name
    pub name: String,
    /// Package version
    pub version: Option<Version>,
    /// Package description
    pub description: Option<String>,
    /// Package authors
    pub authors: Vec<String>,
    /// Package requirements
    pub requires: Vec<String>,
    /// Build requirements
    pub build_requires: Vec<String>,
    /// Private build requirements
    pub private_build_requires: Vec<String>,
    /// Package variants
    pub variants: Vec<Vec<String>>,
    /// Package tools
    pub tools: Vec<String>,
    /// Package commands (rex script string, set from `def commands():` body)
    pub commands: Option<String>,
    /// Package commands function body (alias for commands; used by validation layer)
    pub commands_function: Option<String>,
    /// Build command for custom builds
    pub build_command: Option<String>,
    /// Build system type
    pub build_system: Option<String>,
    /// Pre commands (executed before main commands)
    pub pre_commands: Option<String>,
    /// Post commands (executed after main commands)
    pub post_commands: Option<String>,
    /// Pre test commands (executed before tests)
    pub pre_test_commands: Option<String>,
    /// Pre build commands (executed before build)
    pub pre_build_commands: Option<String>,
    /// Package tests
    pub tests: HashMap<String, PackageTestDefinition>,
    /// Required rez version
    pub requires_rez_version: Option<String>,
    /// Package UUID
    pub uuid: Option<String>,
    /// Package config
    pub config: HashMap<String, String>,
    /// Package help
    pub help: Option<String>,
    /// Package relocatable flag
    pub relocatable: Option<bool>,
    /// Package cachable flag
    pub cachable: Option<bool>,
    /// Package timestamp
    pub timestamp: Option<i64>,
    /// Package revision
    pub revision: Option<String>,
    /// Package changelog
    pub changelog: Option<String>,
    /// Package release message
    pub release_message: Option<String>,
    /// Previous version
    pub previous_version: Option<Version>,
    /// Previous revision
    pub previous_revision: Option<String>,
    /// VCS type
    pub vcs: Option<String>,
    /// Package format version
    pub format_version: Option<i32>,
    /// Package base
    pub base: Option<String>,
    /// Package has plugins
    pub has_plugins: Option<bool>,
    /// Plugin for packages
    pub plugin_for: Vec<String>,
    /// Package hashed variants
    pub hashed_variants: Option<bool>,
    /// Package preprocess function
    pub preprocess: Option<String>,
    /// Whether this is a developer package (loaded from a working directory)
    pub is_dev_package: Option<bool>,
    /// File path to the package definition file (package.py or package.yaml)
    pub filepath: Option<String>,
    /// Set of included Python modules (from @include decorators)
    pub includes: Option<std::collections::HashSet<String>>,
}
