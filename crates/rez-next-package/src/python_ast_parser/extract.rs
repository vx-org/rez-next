//! Extraction helpers: convert AST expressions to typed Rust values.

use super::PythonAstParser;
use super::types::PythonValue;
use crate::package::{PackageTestDefinition, TestCommand, TestVariantFilter};
use rez_next_common::RezCoreError;
use rustpython_ast::{Constant, Expr};
use std::collections::HashMap;

impl PythonAstParser {
    /// Extract a string value from an expression
    pub(super) fn extract_string_value(&self, expr: &Expr) -> Result<String, RezCoreError> {
        match self.evaluate_expression(expr) {
            Ok(PythonValue::String(s)) => Ok(s),
            Ok(PythonValue::Integer(i)) => Ok(i.to_string()),
            Ok(PythonValue::Float(f)) => Ok(f.to_string()),
            Ok(PythonValue::Boolean(b)) => Ok(b.to_string()),
            _ => match expr {
                Expr::Constant(constant) => match &constant.value {
                    Constant::Str(s) => Ok(s.clone()),
                    Constant::Int(i) => Ok(i.to_string()),
                    Constant::Float(f) => Ok(f.to_string()),
                    _ => Err(RezCoreError::PackageParse(format!(
                        "Expected string/number value, got: {:?}",
                        constant.value
                    ))),
                },
                _ => Err(RezCoreError::PackageParse(format!(
                    "Expected constant value, got: {:?}",
                    expr
                ))),
            },
        }
    }

    /// Extract an optional string value from an expression.
    pub(super) fn extract_optional_string(
        &self,
        expr: &Expr,
    ) -> Result<Option<String>, RezCoreError> {
        if matches!(self.evaluate_expression(expr)?, PythonValue::None) {
            Ok(None)
        } else {
            self.extract_string_value(expr).map(Some)
        }
    }

    /// Extract an optional boolean value from an expression
    pub(super) fn extract_bool_value(&self, expr: &Expr) -> Result<Option<bool>, RezCoreError> {
        match self.evaluate_expression(expr) {
            Ok(PythonValue::Boolean(b)) => Ok(Some(b)),
            Ok(PythonValue::None) => Ok(None),
            _ => match expr {
                Expr::Constant(constant) => match &constant.value {
                    Constant::Bool(b) => Ok(Some(*b)),
                    Constant::None => Ok(None),
                    _ => Err(RezCoreError::PackageParse(format!(
                        "Expected boolean value, got: {:?}",
                        constant.value
                    ))),
                },
                _ => Err(RezCoreError::PackageParse(format!(
                    "Expected constant value, got: {:?}",
                    expr
                ))),
            },
        }
    }

    /// Extract an integer value from an expression
    pub(super) fn extract_int_value(&self, expr: &Expr) -> Result<i32, RezCoreError> {
        match self.evaluate_expression(expr) {
            Ok(PythonValue::Integer(i)) => Ok(i as i32),
            _ => match expr {
                Expr::Constant(constant) => match &constant.value {
                    Constant::Int(i) => i.to_string().parse::<i32>().map_err(|e| {
                        RezCoreError::PackageParse(format!("Integer too large for i32: {}", e))
                    }),
                    _ => Err(RezCoreError::PackageParse(format!(
                        "Expected integer value, got: {:?}",
                        constant.value
                    ))),
                },
                _ => Err(RezCoreError::PackageParse(format!(
                    "Expected constant value, got: {:?}",
                    expr
                ))),
            },
        }
    }

    /// Extract a 64-bit integer value from an expression.
    pub(super) fn extract_i64_value(&self, expr: &Expr) -> Result<i64, RezCoreError> {
        match self.evaluate_expression(expr) {
            Ok(PythonValue::Integer(value)) => Ok(value),
            _ => match expr {
                Expr::Constant(constant) => match &constant.value {
                    Constant::Int(value) => value.to_string().parse::<i64>().map_err(|error| {
                        RezCoreError::PackageParse(format!("Integer too large for i64: {error}"))
                    }),
                    _ => Err(RezCoreError::PackageParse(format!(
                        "Expected integer value, got: {:?}",
                        constant.value
                    ))),
                },
                _ => Err(RezCoreError::PackageParse(format!(
                    "Expected constant value, got: {:?}",
                    expr
                ))),
            },
        }
    }

    /// Extract a list of strings from an expression
    pub(super) fn extract_string_list(&self, expr: &Expr) -> Result<Vec<String>, RezCoreError> {
        match self.evaluate_expression(expr) {
            Ok(value @ PythonValue::List(_)) => Self::string_list_from_value(value),
            _ => match expr {
                Expr::List(list) => {
                    let mut result = Vec::new();
                    for elt in &list.elts {
                        result.push(self.extract_string_value(elt)?);
                    }
                    Ok(result)
                }
                Expr::Tuple(tuple) => {
                    let mut result = Vec::new();
                    for elt in &tuple.elts {
                        result.push(self.extract_string_value(elt)?);
                    }
                    Ok(result)
                }
                _ => Err(RezCoreError::PackageParse(format!(
                    "Expected list, got: {:?}",
                    expr
                ))),
            },
        }
    }

    pub(super) fn string_list_from_value(value: PythonValue) -> Result<Vec<String>, RezCoreError> {
        let PythonValue::List(items) = value else {
            return Err(RezCoreError::PackageParse(
                "Expected a list of strings".to_string(),
            ));
        };
        items
            .into_iter()
            .map(|item| match item {
                PythonValue::String(value) => Ok(value),
                PythonValue::Integer(value) => Ok(value.to_string()),
                PythonValue::Float(value) => Ok(value.to_string()),
                PythonValue::Boolean(value) => Ok(value.to_string()),
                _ => Err(RezCoreError::PackageParse(
                    "List contains non-string values".to_string(),
                )),
            })
            .collect()
    }

    /// Extract variants — a list of lists of strings
    pub(super) fn extract_variants(&self, expr: &Expr) -> Result<Vec<Vec<String>>, RezCoreError> {
        match expr {
            Expr::List(list) => {
                let mut result = Vec::new();
                for elt in &list.elts {
                    result.push(self.extract_string_list(elt)?);
                }
                Ok(result)
            }
            _ => Err(RezCoreError::PackageParse(format!(
                "Expected list of lists for variants, got: {:?}",
                expr
            ))),
        }
    }

    /// Extract Rez-compatible test definitions.
    pub(super) fn extract_tests(
        &self,
        expr: &Expr,
    ) -> Result<HashMap<String, PackageTestDefinition>, RezCoreError> {
        let PythonValue::Dict(definitions) = self.evaluate_expression(expr)? else {
            return Err(RezCoreError::PackageParse(format!(
                "Expected dictionary for tests, got: {:?}",
                expr
            )));
        };

        definitions
            .into_iter()
            .map(|(name, value)| {
                self.extract_test_definition(&name, value)
                    .map(|definition| (name, definition))
            })
            .collect()
    }

    fn extract_test_definition(
        &self,
        name: &str,
        value: PythonValue,
    ) -> Result<PackageTestDefinition, RezCoreError> {
        match value {
            PythonValue::String(command) => Ok(PackageTestDefinition::from_command(
                TestCommand::String(command),
            )),
            PythonValue::List(arguments) => Ok(PackageTestDefinition::from_command(
                TestCommand::List(Self::string_list_from_value(PythonValue::List(arguments))?),
            )),
            PythonValue::Dict(mut definition) => {
                let command = definition.remove("command").ok_or_else(|| {
                    RezCoreError::PackageParse(format!(
                        "Structured test definition for '{name}' is missing 'command'"
                    ))
                })?;
                let command = Self::test_command_from_value(name, command)?;
                let requires = definition
                    .remove("requires")
                    .map(|value| Self::test_string_list_from_value(name, "requires", value))
                    .transpose()?
                    .unwrap_or_default();
                let run_on = definition
                    .remove("run_on")
                    .map(|value| Self::test_run_on_from_value(name, value))
                    .transpose()?
                    .unwrap_or_else(|| vec!["default".to_string()]);
                let on_variants = definition
                    .remove("on_variants")
                    .map(|value| Self::test_variant_filter_from_value(name, value))
                    .transpose()?;

                Ok(PackageTestDefinition {
                    command,
                    requires,
                    run_on,
                    on_variants,
                })
            }
            _ => Err(RezCoreError::PackageParse(format!(
                "Unsupported test definition for '{name}'"
            ))),
        }
    }

    fn test_command_from_value(
        name: &str,
        value: PythonValue,
    ) -> Result<TestCommand, RezCoreError> {
        match value {
            PythonValue::String(command) => Ok(TestCommand::String(command)),
            PythonValue::List(arguments) => Ok(TestCommand::List(Self::string_list_from_value(
                PythonValue::List(arguments),
            )?)),
            _ => Err(RezCoreError::PackageParse(format!(
                "Test command for '{name}' must be a string or list of strings"
            ))),
        }
    }

    fn test_string_list_from_value(
        name: &str,
        field: &str,
        value: PythonValue,
    ) -> Result<Vec<String>, RezCoreError> {
        match value {
            PythonValue::List(values) => values
                .into_iter()
                .map(|value| match value {
                    PythonValue::String(value) => Ok(value),
                    _ => Err(RezCoreError::PackageParse(format!(
                        "Test '{name}' field '{field}' must contain only strings"
                    ))),
                })
                .collect(),
            _ => Err(RezCoreError::PackageParse(format!(
                "Test '{name}' field '{field}' must be a list of strings"
            ))),
        }
    }

    fn test_run_on_from_value(name: &str, value: PythonValue) -> Result<Vec<String>, RezCoreError> {
        match value {
            PythonValue::String(tag) => Ok(vec![tag]),
            value => Self::test_string_list_from_value(name, "run_on", value),
        }
    }

    fn test_variant_filter_from_value(
        name: &str,
        value: PythonValue,
    ) -> Result<TestVariantFilter, RezCoreError> {
        match value {
            PythonValue::Boolean(enabled) => Ok(TestVariantFilter::Enabled(enabled)),
            PythonValue::Dict(mut filter) => {
                let kind = match filter.remove("type") {
                    Some(PythonValue::String(kind)) => kind,
                    _ => {
                        return Err(RezCoreError::PackageParse(format!(
                            "Test '{name}' on_variants filter requires type='requires'"
                        )));
                    }
                };
                if kind != "requires" {
                    return Err(RezCoreError::PackageParse(format!(
                        "Unsupported test '{name}' on_variants type '{kind}'"
                    )));
                }
                let value = filter.remove("value").ok_or_else(|| {
                    RezCoreError::PackageParse(format!(
                        "Test '{name}' on_variants requires a value list"
                    ))
                })?;
                Ok(TestVariantFilter::Requires(
                    Self::test_string_list_from_value(name, "on_variants.value", value)?,
                ))
            }
            _ => Err(RezCoreError::PackageParse(format!(
                "Test '{name}' on_variants must be a boolean or requires filter"
            ))),
        }
    }
}
