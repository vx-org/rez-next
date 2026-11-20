//! Core types used throughout the Python AST parser.

use std::collections::HashMap;

use crate::package::PackageTestDefinition;

/// Context for tracking variables and imports during parsing
#[derive(Debug, Default)]
pub(crate) struct ParsingContext {
    /// Variables defined in the current scope
    pub variables: HashMap<String, PythonValue>,
    /// Imported modules and their aliases
    pub imports: HashMap<String, String>,
}

/// Represents a Python value that can be evaluated
#[derive(Debug, Clone)]
pub(crate) enum PythonValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    List(Vec<PythonValue>),
    Dict(HashMap<String, PythonValue>),
    /// Process environment key lookup without materializing variable values.
    Environment,
    /// Rez ephemeral context lookup, resolved after dependency solving.
    Ephemerals,
    /// A deferred membership check against the final ephemeral context.
    EphemeralMembership {
        name: String,
        expected: bool,
    },
    None,
}

impl PythonValue {
    pub(crate) fn into_json_value(self) -> serde_json::Value {
        match self {
            Self::String(value) => serde_json::Value::String(value),
            Self::Integer(value) => serde_json::Value::Number(value.into()),
            Self::Float(value) => serde_json::Number::from_f64(value)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Self::Boolean(value) => serde_json::Value::Bool(value),
            Self::List(values) => {
                serde_json::Value::Array(values.into_iter().map(Self::into_json_value).collect())
            }
            Self::Dict(values) => serde_json::Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, value.into_json_value()))
                    .collect(),
            ),
            Self::None => serde_json::Value::Null,
            value => serde_json::Value::String(format!("{value:?}")),
        }
    }
}

/// Intermediate data structure for collecting package information during parsing
#[derive(Debug, Default)]
pub(crate) struct PackageData {
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub build_command: Option<String>,
    pub build_system: Option<String>,
    pub pre_commands: Option<String>,
    pub post_commands: Option<String>,
    pub pre_test_commands: Option<String>,
    pub pre_build_commands: Option<String>,
    pub tests: HashMap<String, PackageTestDefinition>,
    pub requires_rez_version: Option<String>,
    pub uuid: Option<String>,
    pub authors: Vec<String>,
    pub requires: Vec<String>,
    pub build_requires: Vec<String>,
    pub private_build_requires: Vec<String>,
    pub tools: Vec<String>,
    pub variants: Vec<Vec<String>>,
    pub help: Option<String>,
    pub relocatable: Option<bool>,
    pub cachable: Option<bool>,
    pub commands_function: Option<String>,
    pub extra_fields: HashMap<String, serde_json::Value>,
    // New fields for complete rez compatibility
    pub base: Option<String>,
    pub hashed_variants: Option<bool>,
    pub has_plugins: Option<bool>,
    pub plugin_for: Vec<String>,
    pub format_version: Option<i32>,
    pub preprocess: Option<String>,
    pub timestamp: Option<i64>,
    pub revision: Option<String>,
    pub changelog: Option<String>,
    pub release_message: Option<String>,
    pub previous_version: Option<String>,
    pub previous_revision: Option<String>,
    pub vcs: Option<String>,
}

impl PackageData {
    pub fn new() -> Self {
        Self::default()
    }
}
