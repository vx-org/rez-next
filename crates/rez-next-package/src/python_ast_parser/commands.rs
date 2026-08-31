//! Processing logic for package command function bodies.
//! Supports the full Rex DSL used by rez package commands.

use super::PythonAstParser;
use super::types::{PackageData, PythonValue};
use crate::deferred_commands::{
    defer_build_mode, defer_resolve_membership, defer_unsupported_build_mode,
    defer_unsupported_build_or_ephemeral,
};
use rez_next_common::RezCoreError;
use rustpython_ast::{CmpOp, Expr, Stmt, UnaryOp};

impl PythonAstParser {
    /// Process the `commands` function body
    pub(super) fn process_commands_function(
        &mut self,
        body: &[Stmt],
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        package_data.commands_function = self.process_command_function_body(body)?;
        Ok(())
    }

    /// Process the `pre_commands` function body
    pub(super) fn process_pre_commands_function(
        &mut self,
        body: &[Stmt],
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        package_data.pre_commands = self.process_command_function_body(body)?;
        Ok(())
    }

    /// Process the `post_commands` function body
    pub(super) fn process_post_commands_function(
        &mut self,
        body: &[Stmt],
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        package_data.post_commands = self.process_command_function_body(body)?;
        Ok(())
    }

    /// Process the `pre_test_commands` function body.
    pub(super) fn process_pre_test_commands_function(
        &mut self,
        body: &[Stmt],
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        package_data.pre_test_commands = self.process_command_function_body(body)?;
        Ok(())
    }

    fn process_command_function_body(
        &mut self,
        body: &[Stmt],
    ) -> Result<Option<String>, RezCoreError> {
        let outer_variables = self.context.variables.clone();
        let outer_imports = self.context.imports.clone();
        let result = (|| {
            let mut commands = Vec::new();
            for stmt in body {
                if let Some(command) = self.extract_command_from_statement(stmt)? {
                    commands.push(command);
                }
            }
            Ok((!commands.is_empty()).then(|| commands.join("\n")))
        })();
        self.context.variables = outer_variables;
        self.context.imports = outer_imports;
        result
    }

    fn commands_from_statements(&mut self, statements: &[Stmt]) -> Result<String, RezCoreError> {
        let mut commands = Vec::new();
        for statement in statements {
            if let Some(command) = self.extract_command_from_statement(statement)? {
                commands.push(command);
            }
        }
        Ok(commands.join("\n"))
    }

    fn commands_from_scoped_statements(
        &mut self,
        statements: &[Stmt],
    ) -> Result<String, RezCoreError> {
        let outer_variables = self.context.variables.clone();
        let outer_imports = self.context.imports.clone();
        let result = self.commands_from_statements(statements);
        self.context.variables = outer_variables;
        self.context.imports = outer_imports;
        result
    }

    fn defer_condition_branches(
        &mut self,
        body: &[Stmt],
        orelse: &[Stmt],
        defer: impl Fn(bool, &str) -> String,
    ) -> Result<Option<String>, RezCoreError> {
        let mut commands = Vec::new();
        let body_commands = self.commands_from_scoped_statements(body)?;
        if !body_commands.is_empty() {
            commands.push(defer(true, &body_commands));
        }
        let else_commands = self.commands_from_scoped_statements(orelse)?;
        if !else_commands.is_empty() {
            commands.push(defer(false, &else_commands));
        }
        Ok((!commands.is_empty()).then(|| commands.join("\n")))
    }

    fn resolve_membership_condition(
        &self,
        expression: &Expr,
    ) -> Result<Option<(String, bool)>, RezCoreError> {
        let Expr::Compare(comparison) = expression else {
            return Ok(None);
        };
        if comparison.ops.len() != 1 || comparison.comparators.len() != 1 {
            return Ok(None);
        }
        let Expr::Name(container) = &comparison.comparators[0] else {
            return Ok(None);
        };
        if container.id.as_str() != "resolve" {
            return Ok(None);
        }
        let expected_present = match comparison.ops[0] {
            CmpOp::In => true,
            CmpOp::NotIn => false,
            _ => return Ok(None),
        };
        Ok(Some((
            self.extract_string_value(&comparison.left)?,
            expected_present,
        )))
    }

    fn building_condition(expression: &Expr) -> Option<bool> {
        match expression {
            Expr::Name(name) if name.id.as_str() == "building" => Some(true),
            Expr::UnaryOp(unary)
                if unary.op == UnaryOp::Not
                    && matches!(&*unary.operand, Expr::Name(name) if name.id.as_str() == "building") =>
            {
                Some(false)
            }
            _ => None,
        }
    }

    fn build_or_ephemeral_condition(&self, expression: &Expr) -> Option<String> {
        let Expr::BoolOp(boolean) = expression else {
            return None;
        };
        if boolean.op != rustpython_ast::BoolOp::Or || boolean.values.len() != 2 {
            return None;
        }
        let mut building = false;
        let mut ephemeral = None;
        for value in &boolean.values {
            if Self::building_condition(value) == Some(true) {
                building = true;
            } else if let Expr::Name(name) = value
                && let Some(PythonValue::EphemeralMembership {
                    name: ephemeral_name,
                    expected: true,
                }) = self.context.variables.get(name.id.as_str())
            {
                ephemeral = Some(ephemeral_name.clone());
            }
        }
        if building { ephemeral } else { None }
    }

    /// Extract a Rex DSL command string from a single statement.
    ///
    /// Supports:
    /// - `env.VAR = "value"` (attribute assignment shorthand)
    /// - `env.setenv / prepend_path / append_path / unsetenv / setenv_if_empty`
    /// - `env.VAR.prepend / env.VAR.append / env.VAR.set / env.VAR.unset`
    /// - Top-level: `alias`, `command`, `source`, `info`, `error`, `stop`, `resetenv`
    /// - Top-level shorthand: `setenv`, `prependenv`, `appendenv`, `unsetenv`
    pub(super) fn extract_command_from_statement(
        &mut self,
        stmt: &Stmt,
    ) -> Result<Option<String>, RezCoreError> {
        match stmt {
            Stmt::If(if_stmt) => {
                if let Some(ephemeral_name) = self.build_or_ephemeral_condition(&if_stmt.test) {
                    if !if_stmt.orelse.is_empty() {
                        return Err(RezCoreError::PackageParse(
                            "Dynamic build/ephemeral command guards with else blocks are unsupported"
                                .to_string(),
                        ));
                    }
                    return Ok(Some(defer_unsupported_build_or_ephemeral(&ephemeral_name)));
                }
                if let Some((package_name, present_when_true)) =
                    self.resolve_membership_condition(&if_stmt.test)?
                {
                    return self.defer_condition_branches(
                        &if_stmt.body,
                        &if_stmt.orelse,
                        |condition_is_true, commands| {
                            defer_resolve_membership(
                                commands,
                                &package_name,
                                if condition_is_true {
                                    present_when_true
                                } else {
                                    !present_when_true
                                },
                            )
                        },
                    );
                }
                if let Some(building_when_true) = Self::building_condition(&if_stmt.test) {
                    let deferred = self.defer_condition_branches(
                        &if_stmt.body,
                        &if_stmt.orelse,
                        |condition_is_true, commands| {
                            defer_build_mode(
                                commands,
                                if condition_is_true {
                                    building_when_true
                                } else {
                                    !building_when_true
                                },
                            )
                        },
                    );
                    return match deferred {
                        Ok(commands) => Ok(commands),
                        Err(_) if building_when_true && if_stmt.orelse.is_empty() => {
                            Ok(Some(defer_unsupported_build_mode()))
                        }
                        Err(error) => Err(error),
                    };
                }
                let statements = match self.evaluate_expression(&if_stmt.test)? {
                    super::types::PythonValue::Boolean(true) => &if_stmt.body,
                    super::types::PythonValue::Boolean(false) => &if_stmt.orelse,
                    _ => {
                        return Err(RezCoreError::PackageParse(
                            "Command condition is not statically boolean".to_string(),
                        ));
                    }
                };
                let commands = self.commands_from_statements(statements)?;
                if !commands.is_empty() {
                    return Ok(Some(commands));
                }
            }

            // Handle `env.VAR = "value"` (attribute assignment shorthand)
            Stmt::Assign(assign) => {
                if let Some(Expr::Name(name)) = assign.targets.first() {
                    let value = self.evaluate_expression(&assign.value)?;
                    self.context.variables.insert(name.id.to_string(), value);
                    return Ok(None);
                }
                if let Some(Expr::Attribute(attr)) = assign.targets.first()
                    && let Expr::Name(name_expr) = &*attr.value
                    && name_expr.id.as_str() == "env"
                {
                    let var_name = attr.attr.as_str();
                    if let Ok(value) = self.extract_string_value(&assign.value) {
                        return Ok(Some(format!("env.setenv('{}', '{}')", var_name, value)));
                    }
                }
            }

            // Handle function calls: env.method(...) and top-level Rex calls
            Stmt::Expr(expr_stmt) => {
                if let Expr::Call(call) = &*expr_stmt.value {
                    if let Expr::Attribute(attr) = &*call.func {
                        // ─── env.method('VAR', ...) ───────────────────────────
                        if let Expr::Name(obj) = &*attr.value
                            && obj.id.as_str() == "env"
                        {
                            let method = attr.attr.as_str();
                            match method {
                                "setenv" if call.args.len() >= 2 => {
                                    if let (Ok(k), Ok(v)) = (
                                        self.extract_string_value(&call.args[0]),
                                        self.extract_string_value(&call.args[1]),
                                    ) {
                                        return Ok(Some(format!("env.setenv('{}', '{}')", k, v)));
                                    }
                                }
                                "unsetenv" => {
                                    if let Some(arg) = call.args.first()
                                        && let Ok(k) = self.extract_string_value(arg)
                                    {
                                        return Ok(Some(format!("env.unsetenv('{}')", k)));
                                    }
                                }
                                "prepend_path" if call.args.len() >= 2 => {
                                    if let (Ok(k), Ok(v)) = (
                                        self.extract_string_value(&call.args[0]),
                                        self.extract_string_value(&call.args[1]),
                                    ) {
                                        return Ok(Some(format!(
                                            "env.prepend_path('{}', '{}')",
                                            k, v
                                        )));
                                    }
                                }
                                "append_path" if call.args.len() >= 2 => {
                                    if let (Ok(k), Ok(v)) = (
                                        self.extract_string_value(&call.args[0]),
                                        self.extract_string_value(&call.args[1]),
                                    ) {
                                        return Ok(Some(format!(
                                            "env.append_path('{}', '{}')",
                                            k, v
                                        )));
                                    }
                                }
                                "setenv_if_empty" if call.args.len() >= 2 => {
                                    if let (Ok(k), Ok(v)) = (
                                        self.extract_string_value(&call.args[0]),
                                        self.extract_string_value(&call.args[1]),
                                    ) {
                                        return Ok(Some(format!(
                                            "env.setenv_if_empty('{}', '{}')",
                                            k, v
                                        )));
                                    }
                                }
                                _ => {}
                            }
                        }

                        // ─── env.VAR.prepend / env.VAR.append ─────────────────
                        if let Expr::Attribute(env_attr) = &*attr.value
                            && let Expr::Name(obj) = &*env_attr.value
                            && obj.id.as_str() == "env"
                        {
                            let var_name = env_attr.attr.as_str();
                            let method = attr.attr.as_str();
                            if let Some(arg) = call.args.first()
                                && let Ok(value) = self.extract_string_value(arg)
                            {
                                match method {
                                    "prepend" => {
                                        return Ok(Some(format!(
                                            "env.prepend_path('{}', '{}')",
                                            var_name, value
                                        )));
                                    }
                                    "append" => {
                                        return Ok(Some(format!(
                                            "env.append_path('{}', '{}')",
                                            var_name, value
                                        )));
                                    }
                                    "set" => {
                                        return Ok(Some(format!(
                                            "env.setenv('{}', '{}')",
                                            var_name, value
                                        )));
                                    }
                                    "unset" => {
                                        return Ok(Some(format!("env.unsetenv('{}')", var_name)));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    // ─── Top-level Rex calls ──────────────────────────────────
                    if let Expr::Name(func_name) = &*call.func {
                        match func_name.id.as_str() {
                            "alias" if call.args.len() >= 2 => {
                                if let (Ok(name), Ok(cmd)) = (
                                    self.extract_string_value(&call.args[0]),
                                    self.extract_string_value(&call.args[1]),
                                ) {
                                    return Ok(Some(format!("alias('{}', '{}')", name, cmd)));
                                }
                            }
                            "command" => {
                                if let Some(arg) = call.args.first()
                                    && let Ok(cmd) = self.extract_string_value(arg)
                                {
                                    return Ok(Some(format!("command('{}')", cmd)));
                                }
                            }
                            "source" => {
                                if let Some(arg) = call.args.first()
                                    && let Ok(path) = self.extract_string_value(arg)
                                {
                                    return Ok(Some(format!("source('{}')", path)));
                                }
                            }
                            "info" => {
                                if let Some(arg) = call.args.first()
                                    && let Ok(msg) = self.extract_string_value(arg)
                                {
                                    return Ok(Some(format!("info('{}')", msg)));
                                }
                            }
                            "error" => {
                                if let Some(arg) = call.args.first()
                                    && let Ok(msg) = self.extract_string_value(arg)
                                {
                                    return Ok(Some(format!("error('{}')", msg)));
                                }
                            }
                            "stop" => {
                                if call.args.is_empty() {
                                    return Ok(Some("stop()".to_string()));
                                } else if let Ok(msg) = self.extract_string_value(&call.args[0]) {
                                    return Ok(Some(format!("stop('{}')", msg)));
                                }
                            }
                            "resetenv" => {
                                if let Some(arg) = call.args.first()
                                    && let Ok(var) = self.extract_string_value(arg)
                                {
                                    return Ok(Some(format!("resetenv('{}')", var)));
                                }
                            }
                            "setenv" if call.args.len() >= 2 => {
                                if let (Ok(k), Ok(v)) = (
                                    self.extract_string_value(&call.args[0]),
                                    self.extract_string_value(&call.args[1]),
                                ) {
                                    return Ok(Some(format!("setenv('{}', '{}')", k, v)));
                                }
                            }
                            "prependenv" if call.args.len() >= 2 => {
                                if let (Ok(k), Ok(v)) = (
                                    self.extract_string_value(&call.args[0]),
                                    self.extract_string_value(&call.args[1]),
                                ) {
                                    return Ok(Some(format!("prependenv('{}', '{}')", k, v)));
                                }
                            }
                            "appendenv" if call.args.len() >= 2 => {
                                if let (Ok(k), Ok(v)) = (
                                    self.extract_string_value(&call.args[0]),
                                    self.extract_string_value(&call.args[1]),
                                ) {
                                    return Ok(Some(format!("appendenv('{}', '{}')", k, v)));
                                }
                            }
                            "unsetenv" => {
                                if let Some(arg) = call.args.first()
                                    && let Ok(k) = self.extract_string_value(arg)
                                {
                                    return Ok(Some(format!("unsetenv('{}')", k)));
                                }
                            }
                            _ => {}
                        }
                    }

                    return Err(RezCoreError::PackageParse(format!(
                        "Unsupported command statement: {call:?}"
                    )));
                }
            }
            Stmt::Import(import) => self.process_import_statement(import)?,
            Stmt::ImportFrom(import) => self.process_import_from_statement(import)?,
            Stmt::Pass(_) => {}
            _ => {
                return Err(RezCoreError::PackageParse(format!(
                    "Unsupported command statement: {stmt:?}"
                )));
            }
        }

        Ok(None)
    }
}
