//! Advanced Python AST parser for package.py files using RustPython.
//!
//! This module is split into focused submodules:
//! - `types`: Core data types (`PythonValue`, `ParsingContext`, `PackageData`)
//! - `eval`: Expression evaluation (`evaluate_*`)
//! - `extract`: Typed value extraction (`extract_*`)
//! - `commands`: Rex DSL command parsing (`process_commands_function` etc.)

mod commands;
mod eval;
mod extract;
mod metadata;
mod types;

use crate::Package;
use rez_next_common::RezCoreError;
use rez_next_version::Version;
use rustpython_ast::{Expr, Stmt, Suite};
use rustpython_parser::Parse;
use types::{PackageData, ParsingContext};

/// Advanced Python AST parser for package.py files
#[derive(Default)]
pub struct PythonAstParser {
    /// Context for tracking variables and imports during parsing
    pub(crate) context: ParsingContext,
}

impl PythonAstParser {
    /// Create a new parser instance
    pub fn new() -> Self {
        Self {
            context: ParsingContext::default(),
        }
    }

    /// Parse a package.py file using Python AST.
    pub fn parse_package_py(content: &str) -> Result<Package, RezCoreError> {
        let mut parser = Self::new();
        parser.parse_package_py_with_context(content)
    }

    /// Parse a package.py file with context tracking
    fn parse_package_py_with_context(&mut self, content: &str) -> Result<Package, RezCoreError> {
        let ast = Suite::parse(content, "package.py")
            .map_err(|e| RezCoreError::PackageParse(format!("Python syntax error: {}", e)))?;

        let mut package_data = PackageData::new();

        for stmt in &ast {
            self.process_statement(stmt, &mut package_data)?;
        }

        Self::build_package(package_data)
    }

    /// Process a single AST statement
    fn process_statement(
        &mut self,
        stmt: &Stmt,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        match stmt {
            Stmt::Assign(assign) => {
                if let Some(Expr::Name(name_expr)) = assign.targets.first() {
                    self.process_assignment(&name_expr.id, &assign.value, package_data)?;
                }
            }
            Stmt::FunctionDef(func_def) => {
                self.process_function_definition(func_def, package_data)?;
            }
            Stmt::Import(import) => {
                self.process_import_statement(import)?;
            }
            Stmt::ImportFrom(import_from) => {
                self.process_import_from_statement(import_from)?;
            }
            Stmt::If(if_stmt) => {
                self.process_if_statement(if_stmt, package_data)?;
            }
            Stmt::For(for_stmt) => {
                self.process_for_statement(for_stmt, package_data)?;
            }
            Stmt::While(while_stmt) => {
                self.process_while_statement(while_stmt, package_data)?;
            }
            Stmt::Try(try_stmt) => {
                self.process_try_statement(try_stmt, package_data)?;
            }
            Stmt::With(with_stmt) => {
                self.process_with_statement(with_stmt, package_data)?;
            }
            Stmt::Expr(expr_stmt) => {
                self.process_expression_statement(&expr_stmt.value, package_data)?;
            }
            Stmt::Pass(_) => {}
            _ => {
                return Err(RezCoreError::PackageParse(format!(
                    "Unsupported package.py statement: {stmt:?}"
                )));
            }
        }
        Ok(())
    }

    /// Process import statements (`import os`)
    fn process_import_statement(
        &mut self,
        import: &rustpython_ast::StmtImport,
    ) -> Result<(), RezCoreError> {
        for alias in &import.names {
            let module_name = alias.name.as_str();
            let alias_name = alias
                .asname
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(module_name);
            self.context
                .imports
                .insert(alias_name.to_string(), module_name.to_string());
        }
        Ok(())
    }

    /// Process from-import statements (`from os import path`)
    fn process_import_from_statement(
        &mut self,
        import_from: &rustpython_ast::StmtImportFrom,
    ) -> Result<(), RezCoreError> {
        if let Some(module) = &import_from.module {
            for alias in &import_from.names {
                let name = alias.name.as_str();
                let alias_name = alias.asname.as_ref().map(|s| s.as_str()).unwrap_or(name);
                let full_name = format!("{}.{}", module, name);
                self.context
                    .imports
                    .insert(alias_name.to_string(), full_name);
            }
        }
        Ok(())
    }

    /// Process function definitions (`def commands(): ...`)
    fn process_function_definition(
        &mut self,
        func_def: &rustpython_ast::StmtFunctionDef,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        match func_def.name.as_str() {
            "commands" => {
                self.process_commands_function(&func_def.body, package_data)?;
            }
            "pre_commands" => {
                if !Self::function_has_decorator(func_def, "include") {
                    self.process_pre_commands_function(&func_def.body, package_data)?;
                }
            }
            "post_commands" => {
                self.process_post_commands_function(&func_def.body, package_data)?;
            }
            "pre_test_commands" => {
                self.process_pre_test_commands_function(&func_def.body, package_data)?;
            }
            "requires" => {
                self.process_late_requires_function(func_def, package_data)?;
            }
            _ => {
                return Err(RezCoreError::PackageParse(format!(
                    "Unsupported package.py function: {}",
                    func_def.name
                )));
            }
        }
        Ok(())
    }

    fn function_has_decorator(
        function: &rustpython_ast::StmtFunctionDef,
        decorator_name: &str,
    ) -> bool {
        function.decorator_list.iter().any(|decorator| {
            matches!(
                decorator,
                Expr::Call(call)
                    if matches!(&*call.func, Expr::Name(name) if name.id.as_str() == decorator_name)
            )
        })
    }

    /// Process conditional statements
    fn process_if_statement(
        &mut self,
        if_stmt: &rustpython_ast::StmtIf,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        match self.evaluate_expression(&if_stmt.test)? {
            types::PythonValue::Boolean(true) => {
                for stmt in &if_stmt.body {
                    self.process_statement(stmt, package_data)?;
                }
            }
            types::PythonValue::Boolean(false) => {
                for stmt in &if_stmt.orelse {
                    self.process_statement(stmt, package_data)?;
                }
            }
            _ => {
                return Err(RezCoreError::PackageParse(
                    "Package condition is not statically boolean".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Process variable assignments and map them to `PackageData` fields
    fn process_assignment(
        &mut self,
        var_name: &str,
        value: &Expr,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        let python_value = self.evaluate_expression(value)?;
        self.context
            .variables
            .insert(var_name.to_string(), python_value.clone());

        match var_name {
            "name" => {
                package_data.name = Some(self.extract_string_value(value)?);
            }
            "version" => {
                package_data.version = Some(self.extract_string_value(value)?);
            }
            "description" => {
                package_data.description = Some(self.extract_string_value(value)?);
            }
            "build_command" => {
                package_data.build_command = Some(self.extract_string_value(value)?);
            }
            "build_system" => {
                package_data.build_system = Some(self.extract_string_value(value)?);
            }
            "uuid" => {
                package_data.uuid = Some(self.extract_string_value(value)?);
            }
            "authors" => {
                package_data.authors = self.extract_string_list(value)?;
            }
            "requires" => {
                package_data.requires = self.extract_string_list(value)?;
            }
            "build_requires" => {
                package_data.build_requires = self.extract_string_list(value)?;
            }
            "private_build_requires" => {
                package_data.private_build_requires = self.extract_string_list(value)?;
            }
            "tools" => {
                package_data.tools = self.extract_string_list(value)?;
            }
            "variants" => {
                package_data.variants = self.extract_variants(value)?;
            }
            "tests" => {
                package_data.tests = self.extract_tests(value)?;
            }
            "commands" => {
                if let Ok(s) = self.extract_string_value(value) {
                    package_data.commands_function = Some(s);
                }
            }
            "pre_commands" => {
                package_data.pre_commands = Some(self.extract_string_value(value)?);
            }
            "post_commands" => {
                package_data.post_commands = Some(self.extract_string_value(value)?);
            }
            "pre_test_commands" => {
                package_data.pre_test_commands = Some(self.extract_string_value(value)?);
            }
            "pre_build_commands" => {
                package_data.pre_build_commands = Some(self.extract_string_value(value)?);
            }
            "requires_rez_version" => {
                package_data.requires_rez_version = Some(self.extract_string_value(value)?);
            }
            "help" => {
                package_data.help = Some(self.extract_string_value(value)?);
            }
            "relocatable" => {
                package_data.relocatable = self.extract_bool_value(value)?;
            }
            "cachable" => {
                package_data.cachable = self.extract_bool_value(value)?;
            }
            "base" => {
                package_data.base = Some(self.extract_string_value(value)?);
            }
            "hashed_variants" => {
                package_data.hashed_variants = self.extract_bool_value(value)?;
            }
            "has_plugins" => {
                package_data.has_plugins = self.extract_bool_value(value)?;
            }
            "plugin_for" => {
                package_data.plugin_for = self.extract_string_list(value)?;
            }
            "format_version" => {
                package_data.format_version = Some(self.extract_int_value(value)?);
            }
            "preprocess" => {
                package_data.preprocess = Some(self.extract_string_value(value)?);
            }
            "timestamp" => {
                package_data.timestamp = if matches!(python_value, types::PythonValue::None) {
                    None
                } else {
                    Some(self.extract_i64_value(value)?)
                };
            }
            "revision" => {
                package_data.revision = self.extract_optional_string(value)?;
            }
            "changelog" => {
                package_data.changelog = self.extract_optional_string(value)?;
            }
            "release_message" => {
                package_data.release_message = self.extract_optional_string(value)?;
            }
            "previous_version" => {
                package_data.previous_version = self.extract_optional_string(value)?;
            }
            "previous_revision" => {
                package_data.previous_revision = self.extract_optional_string(value)?;
            }
            "vcs" => {
                package_data.vcs = self.extract_optional_string(value)?;
            }
            _ => {
                package_data
                    .extra_fields
                    .insert(var_name.to_string(), python_value.into_json_value());
            }
        }
        Ok(())
    }

    /// Reject loops because traversing their body once corrupts package metadata.
    fn process_for_statement(
        &mut self,
        for_stmt: &rustpython_ast::StmtFor,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        let _ = package_data;
        Err(RezCoreError::PackageParse(format!(
            "Unsupported package.py for loop: {for_stmt:?}"
        )))
    }

    /// Reject loops because traversing their body once corrupts package metadata.
    fn process_while_statement(
        &mut self,
        while_stmt: &rustpython_ast::StmtWhile,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        let _ = package_data;
        Err(RezCoreError::PackageParse(format!(
            "Unsupported package.py while loop: {while_stmt:?}"
        )))
    }

    /// Reject try/except because evaluating every branch corrupts package metadata.
    fn process_try_statement(
        &mut self,
        try_stmt: &rustpython_ast::StmtTry,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        let _ = package_data;
        Err(RezCoreError::PackageParse(format!(
            "Unsupported package.py try statement: {try_stmt:?}"
        )))
    }

    /// Process with statements
    fn process_with_statement(
        &mut self,
        with_stmt: &rustpython_ast::StmtWith,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        let _ = package_data;
        Err(RezCoreError::PackageParse(format!(
            "Unsupported package.py with statement: {with_stmt:?}"
        )))
    }

    /// Process standalone expression statements (evaluate for side-effects only)
    fn process_expression_statement(
        &mut self,
        expr: &Expr,
        _package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        self.evaluate_expression(expr)?;
        Ok(())
    }

    /// Build a `Package` from the collected `PackageData`
    fn build_package(data: PackageData) -> Result<Package, RezCoreError> {
        let name = data
            .name
            .ok_or_else(|| RezCoreError::PackageParse("Missing 'name' field".to_string()))?;

        let mut package = Package::new(name);

        if let Some(version_str) = data.version {
            package.version = Some(
                Version::parse(&version_str)
                    .map_err(|e| RezCoreError::PackageParse(format!("Invalid version: {}", e)))?,
            );
        }

        package.description = data.description;
        package.build_command = data.build_command;
        package.build_system = data.build_system;
        package.pre_commands = data.pre_commands;
        package.post_commands = data.post_commands;
        package.pre_test_commands = data.pre_test_commands;
        package.pre_build_commands = data.pre_build_commands;
        package.tests = data.tests;
        package.requires_rez_version = data.requires_rez_version;
        package.uuid = data.uuid;
        package.authors = data.authors;
        package.requires = data.requires;
        package.build_requires = data.build_requires;
        package.private_build_requires = data.private_build_requires;
        package.tools = data.tools;
        package.variants = data.variants;
        package.help = data.help;
        package.relocatable = data.relocatable;
        package.cachable = data.cachable;
        package.commands = data.commands_function.clone();
        package.commands_function = data.commands_function;
        package.extra_fields = data.extra_fields;
        package.base = data.base;
        package.hashed_variants = data.hashed_variants;
        package.has_plugins = data.has_plugins;
        package.plugin_for = data.plugin_for;
        package.format_version = data.format_version;
        package.preprocess = data.preprocess;
        package.timestamp = data.timestamp;
        package.revision = data.revision;
        package.changelog = data.changelog;
        package.release_message = data.release_message;
        package.previous_version = data
            .previous_version
            .map(|version| {
                Version::parse(&version).map_err(|error| {
                    RezCoreError::PackageParse(format!("Invalid previous version: {error}"))
                })
            })
            .transpose()?;
        package.previous_revision = data.previous_revision;
        package.vcs = data.vcs;

        package.validate()?;

        Ok(package)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TestCommand, TestVariantFilter};

    #[test]
    fn test_parse_package_with_new_fields() {
        let package_py_content = r#"
name = "test_package"
version = "1.0.0"
description = "Test package with new fields"
base = "base_package"
hashed_variants = True
has_plugins = True
plugin_for = ["maya", "nuke"]
format_version = 2
preprocess = "some_preprocess_function"
"#;

        let result = PythonAstParser::parse_package_py(package_py_content);
        assert!(
            result.is_ok(),
            "Failed to parse package.py: {:?}",
            result.err()
        );

        let package = result.unwrap();
        assert_eq!(package.name, "test_package");
        assert_eq!(package.base, Some("base_package".to_string()));
        assert_eq!(package.hashed_variants, Some(true));
        assert_eq!(package.has_plugins, Some(true));
        assert_eq!(package.plugin_for, vec!["maya", "nuke"]);
        assert_eq!(package.format_version, Some(2));
        assert_eq!(
            package.preprocess,
            Some("some_preprocess_function".to_string())
        );
    }

    #[test]
    fn test_parse_package_preserves_arbitrary_fields() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "metadata_package"
version = "1.0.0"
vx_upstream_sha256 = "abc123"
vx_build_options = {"lto": True, "jobs": 8}
vx_targets = ["linux", "windows"]
"#,
        )
        .expect("arbitrary package attributes should parse");

        assert_eq!(
            package.arbitrary_value("vx_upstream_sha256"),
            Some(&serde_json::json!("abc123"))
        );
        assert_eq!(
            package.arbitrary_value("vx_build_options"),
            Some(&serde_json::json!({"lto": true, "jobs": 8}))
        );
        assert_eq!(
            package.arbitrary_keys(),
            vec![
                "vx_build_options".to_string(),
                "vx_targets".to_string(),
                "vx_upstream_sha256".to_string()
            ]
        );
    }

    #[test]
    fn test_parse_package_release_metadata() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "released_package"
version = "2.0.0"
timestamp = 1700000000
revision = "abc123"
changelog = "Changes"
release_message = "Release 2.0.0"
previous_version = "1.9.0"
previous_revision = "def456"
vcs = "git"
"#,
        )
        .expect("release metadata should parse");

        assert_eq!(package.timestamp, Some(1_700_000_000));
        assert_eq!(package.revision.as_deref(), Some("abc123"));
        assert_eq!(package.changelog.as_deref(), Some("Changes"));
        assert_eq!(package.release_message.as_deref(), Some("Release 2.0.0"));
        assert_eq!(
            package.previous_version.as_ref().map(ToString::to_string),
            Some("1.9.0".to_string())
        );
        assert_eq!(package.previous_revision.as_deref(), Some("def456"));
        assert_eq!(package.vcs.as_deref(), Some("git"));
    }

    #[test]
    fn test_parse_package_with_false_boolean_fields() {
        let package_py_content = r#"
name = "test_package"
version = "1.0.0"
hashed_variants = False
has_plugins = False
"#;

        let result = PythonAstParser::parse_package_py(package_py_content);
        assert!(
            result.is_ok(),
            "Failed to parse package.py: {:?}",
            result.err()
        );

        let package = result.unwrap();
        assert_eq!(package.hashed_variants, Some(false));
        assert_eq!(package.has_plugins, Some(false));
    }

    #[test]
    fn test_parse_package_with_conditional_logic() {
        let package_py_content = r#"
name = "test_package"
version = "1.0.0"

import os
if os.name == "nt":
    requires = ["windows-lib"]
else:
    requires = ["unix-lib"]

def commands():
    env.PATH.append("/usr/local/bin")
    env.PYTHONPATH.prepend("/opt/python")
"#;

        let result = PythonAstParser::parse_package_py(package_py_content);
        assert!(
            result.is_ok(),
            "Failed to parse package.py: {:?}",
            result.err()
        );

        let package = result.unwrap();
        assert_eq!(package.name, "test_package");
        assert!(!package.requires.is_empty());
    }

    #[test]
    fn test_parse_package_with_expressions() {
        let package_py_content = r#"
name = "test_package"
version = "1.0.0"

base_version = "2.0"
version = base_version + ".1"

authors = ["author1"] + ["author2"]
"#;

        let result = PythonAstParser::parse_package_py(package_py_content);
        assert!(
            result.is_ok(),
            "Failed to parse package.py: {:?}",
            result.err()
        );

        let package = result.unwrap();
        assert_eq!(package.name, "test_package");
    }

    #[test]
    fn test_commands_select_os_getenv_default_branch() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = "1.0.0"
import os
def commands():
    if os.getenv("REZ_NEXT_TEST_UNSET", "false").lower() == "true":
        env.MODE = "enabled"
    else:
        env.MODE = "disabled"
"#,
        )
        .unwrap();

        assert_eq!(
            package.commands.as_deref(),
            Some("env.setenv('MODE', 'disabled')")
        );
    }

    #[test]
    fn test_commands_use_imports_declared_inside_the_function() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "python"
version = "3.9.13"
def commands():
    from platform import system
    if system() == "Windows":
        env.PLATFORM_MODE = "windows"
    else:
        env.PLATFORM_MODE = "posix"
"#,
        )
        .expect("safe command-scope imports should be available to conditions");

        let expected = if cfg!(windows) { "windows" } else { "posix" };
        assert_eq!(
            package.commands.as_deref(),
            Some(format!("env.setenv('PLATFORM_MODE', '{expected}')").as_str())
        );
    }

    #[test]
    fn test_commands_evaluate_platform_alias_with_local_variables() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = "15.2.1"
def commands():
    extension = ".exe" if system.platform == "windows" else ""
    executable = "Tool{{this.version.major}}.{{this.version.minor}}{0}".format(extension)
    if system.platform == "windows":
        alias("tool", "{0} $*".format(executable))
    else:
        alias("tool", "{0} $@".format(executable))
"#,
        )
        .unwrap();

        let suffix = if cfg!(windows) { ".exe $*" } else { " $@" };
        assert_eq!(
            package.commands.as_deref(),
            Some(format!(
                "alias('tool', 'Tool{{this.version.major}}.{{this.version.minor}}{suffix}')"
            ))
            .as_deref()
        );
    }

    #[test]
    fn test_dynamic_metadata_is_rejected() {
        let error = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = load_version()
"#,
        )
        .expect_err("dynamic metadata must not be converted into debug text");

        assert!(
            error.to_string().contains("Unsupported function call"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_unknown_command_is_rejected() {
        let error = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = "1.0.0"
def commands():
    env.PATH.extend("/unsupported")
"#,
        )
        .expect_err("unsupported commands must not disappear from the environment");

        assert!(
            error.to_string().contains("Unsupported command statement"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_included_dynamic_pre_commands_do_not_block_environment_commands() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "pywin32"
version = "306"
def commands():
    env.PYTHONPATH.prepend("{this.root}/site-packages")
@include("pkg_shared")
def pre_commands():
    for dll in discover_system_dlls():
        copy_to_system(dll)
"#,
        )
        .expect("external pre-command extensions should not block package environments");

        assert!(package.pre_commands.is_none());
        assert_eq!(
            package.commands.as_deref(),
            Some("env.prepend_path('PYTHONPATH', '{this.root}/site-packages')")
        );
    }

    #[test]
    fn test_dynamic_metadata_function_is_rejected() {
        let error = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
@early()
def version():
    return "1.0.0"
"#,
        )
        .expect_err("dynamic metadata functions require Python execution");

        assert!(
            error
                .to_string()
                .contains("Unsupported package.py function"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_late_bound_requires_function_is_evaluated() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "blender"
version = "4.5.5"

@late()
def requires():
    requires_ = ["~python-3.11", "~msvc-14.0", "python_embedded-1"]
    import os
    if "__REZ_NEXT_TEST_LATE_REQUIRES_SHOULD_NOT_EXIST__" in os.environ:
        requires_ = []
    return requires_
"#,
        )
        .expect("supported late-bound requirements should be evaluated safely");

        assert_eq!(
            package.requires,
            ["~python-3.11", "~msvc-14.0", "python_embedded-1"]
        );
    }

    #[test]
    fn test_dynamic_control_flow_is_rejected() {
        let error = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = "1.0.0"
for requirement in discover_requirements():
    requires = [requirement]
"#,
        )
        .expect_err("loops must not be approximated by executing their body once");

        assert!(
            error
                .to_string()
                .contains("Unsupported package.py for loop"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_structured_package_test_command_is_supported() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = "1.0.0"
tests = {
    "unit": {
        "command": "python -m pytest",
        "requires": ["pytest"],
    },
}
"#,
        )
        .expect("structured test metadata should preserve its command");

        assert_eq!(
            package
                .tests
                .get("unit")
                .map(|test| test.command.as_string()),
            Some("python -m pytest".to_string())
        );
    }

    #[test]
    fn test_package_test_definitions_preserve_all_supported_shapes() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = "1.0.0"
tests = {
    "shell": "python -m unittest",
    "argv": ["python", "-m", "pytest"],
    "structured": {
        "command": ["python", "-m", "coverage"],
        "requires": ["coverage-7"],
        "run_on": ["ci", "nightly"],
        "on_variants": {"type": "requires", "value": ["python-3.11"]},
    },
}
"#,
        )
        .unwrap();

        assert!(matches!(
            package.tests["shell"].command,
            TestCommand::String(ref command) if command == "python -m unittest"
        ));
        assert!(matches!(
            package.tests["argv"].command,
            TestCommand::List(ref command) if command == &["python", "-m", "pytest"]
        ));
        let structured = &package.tests["structured"];
        assert_eq!(structured.requires, vec!["coverage-7"]);
        assert_eq!(structured.run_on, vec!["ci", "nightly"]);
        assert_eq!(
            structured.on_variants,
            Some(TestVariantFilter::Requires(vec!["python-3.11".to_string()]))
        );
    }

    #[test]
    fn test_package_test_run_on_accepts_a_single_tag() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
tests = {
    "release": {
        "command": "python -m pytest",
        "run_on": "pre_release",
    },
}
"#,
        )
        .unwrap();

        assert_eq!(
            package.tests["release"].run_on,
            vec!["pre_release".to_string()]
        );
    }

    #[test]
    fn test_package_test_command_preserves_nested_quotes() {
        let package = PythonAstParser::parse_package_py(
            r#"
name = "test_package"
version = "1.0.0"
tests = {
    "import": "python -c \"import test_package; print(test_package.VALUE)\"",
}
"#,
        )
        .unwrap();

        assert_eq!(
            package
                .tests
                .get("import")
                .map(|test| test.command.as_string()),
            Some(r#"python -c "import test_package; print(test_package.VALUE)""#.to_string())
        );
    }
}
