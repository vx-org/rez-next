//! Safe evaluation of supported function-valued package metadata.

use super::PythonAstParser;
use super::types::{PackageData, PythonValue};
use rez_next_common::RezCoreError;
use rustpython_ast::{Expr, Stmt, StmtFunctionDef};

impl PythonAstParser {
    pub(super) fn process_late_requires_function(
        &mut self,
        function: &StmtFunctionDef,
        package_data: &mut PackageData,
    ) -> Result<(), RezCoreError> {
        if !function.decorator_list.iter().any(is_late_decorator) {
            return Err(RezCoreError::PackageParse(
                "Function-valued 'requires' metadata must use @late()".to_string(),
            ));
        }

        let outer_variables = self.context.variables.clone();
        let outer_imports = self.context.imports.clone();
        let result = self.evaluate_metadata_statements(&function.body);
        self.context.variables = outer_variables;
        self.context.imports = outer_imports;

        package_data.requires = Self::string_list_from_value(result?.ok_or_else(|| {
            RezCoreError::PackageParse("Late-bound 'requires' did not return a value".to_string())
        })?)?;
        Ok(())
    }

    fn evaluate_metadata_statements(
        &mut self,
        statements: &[Stmt],
    ) -> Result<Option<PythonValue>, RezCoreError> {
        for statement in statements {
            match statement {
                Stmt::Assign(assignment) => {
                    let Some(Expr::Name(name)) = assignment.targets.first() else {
                        return Err(RezCoreError::PackageParse(
                            "Late metadata only supports local-name assignments".to_string(),
                        ));
                    };
                    let value = self.evaluate_expression(&assignment.value)?;
                    self.context.variables.insert(name.id.to_string(), value);
                }
                Stmt::Import(import) => self.process_import_statement(import)?,
                Stmt::ImportFrom(import) => self.process_import_from_statement(import)?,
                Stmt::If(if_statement) => {
                    let branch = match self.evaluate_expression(&if_statement.test)? {
                        PythonValue::Boolean(true) => &if_statement.body,
                        PythonValue::Boolean(false) => &if_statement.orelse,
                        _ => {
                            return Err(RezCoreError::PackageParse(
                                "Late metadata condition is not statically boolean".to_string(),
                            ));
                        }
                    };
                    if let Some(value) = self.evaluate_metadata_statements(branch)? {
                        return Ok(Some(value));
                    }
                }
                Stmt::Return(return_statement) => {
                    return return_statement
                        .value
                        .as_deref()
                        .map(|value| self.evaluate_expression(value))
                        .transpose();
                }
                Stmt::Expr(expression)
                    if matches!(
                        expression.value.as_ref(),
                        Expr::Constant(constant)
                            if matches!(constant.value, rustpython_ast::Constant::Str(_))
                    ) => {}
                Stmt::Pass(_) => {}
                _ => {
                    return Err(RezCoreError::PackageParse(format!(
                        "Unsupported late metadata statement: {statement:?}"
                    )));
                }
            }
        }
        Ok(None)
    }
}

fn is_late_decorator(decorator: &Expr) -> bool {
    match decorator {
        Expr::Call(call) if call.args.is_empty() && call.keywords.is_empty() => {
            matches!(&*call.func, Expr::Name(name) if name.id.as_str() == "late")
        }
        _ => false,
    }
}
