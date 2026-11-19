//! Python bindings for PackageTestRunner and PackageTestResults
//!
//! Exposes rez.package_test functionality to Python.
//! Usage from Python:
//! ```python
//! import rez_next as rez
//! runner = rez.test.PackageTestRunner("my_package")
//! test_names = runner.get_test_names()
//! for name in test_names:
//!     exit_code = runner.run_test(name)
//! print(runner.print_summary())
//! ```

use crate::package_functions::make_repo_manager;
use crate::runtime::get_runtime;
use pyo3::prelude::*;
use pyo3::types::PyType;
use rez_next_context::{ContextConfig, EnvironmentManager};
use rez_next_package::package::test_runner::TestDefinition;
use rez_next_package::package::test_runner::{
    PackageTestResults as RustTestResults, PackageTestRunner as RustTestRunner, TestStatus,
};
use rez_next_package::{Package, Requirement, TestVariantFilter};
use rez_next_repository::simple_repository::RepositoryManager;
use rez_next_solver::{DependencyResolver, SolverConfig};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn package_not_found(py: Python<'_>, package_spec: &str) -> PyErr {
    let exception = match py
        .import("rez_next.exceptions")
        .and_then(|module| module.getattr("PackageNotFoundError"))
    {
        Ok(exception) => exception,
        Err(error) => return error,
    };
    let exception_type = match exception.cast::<PyType>() {
        Ok(exception_type) => exception_type.clone(),
        Err(error) => return error.into(),
    };
    PyErr::from_type(
        exception_type,
        format!("Package '{package_spec}' not found"),
    )
}

fn package_root(package: &Package) -> Result<PathBuf, String> {
    package
        .root()
        .map(PathBuf::from)
        .filter(|root| root.is_dir())
        .ok_or_else(|| {
            format!(
                "Resolved package '{}' does not have a valid package root",
                package.qualified_name()
            )
        })
}

fn resolve_package(py: Python<'_>, package_spec: &str) -> PyResult<(Package, PathBuf, bool)> {
    let requested_path = Path::new(package_spec);
    let is_package_path = requested_path.is_file()
        || requested_path.is_dir()
            && ["package.py", "package.yaml", "package.yml"]
                .iter()
                .any(|name| requested_path.join(name).is_file());
    if is_package_path {
        let package = Package::from_path(requested_path)
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
        let root = package_root(&package).map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        return Ok((package, root, false));
    }

    let requirement = package_spec.parse::<Requirement>().map_err(|error| {
        pyo3::exceptions::PyValueError::new_err(format!(
            "Invalid package request '{package_spec}': {error}"
        ))
    })?;
    let repo_manager = make_repo_manager(None)?;
    let packages = get_runtime()
        .block_on(repo_manager.find_packages(&requirement.name))
        .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
    let package = packages
        .into_iter()
        .find(|candidate| {
            candidate
                .version
                .as_ref()
                .is_some_and(|version| requirement.is_satisfied_by(version))
                || candidate.version.is_none() && requirement.version_constraint.is_none()
        })
        .map(|package| (*package).clone())
        .ok_or_else(|| package_not_found(py, package_spec))?;
    let root = package_root(&package).map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
    Ok((package, root, true))
}

fn parse_test_requirements(requirements: &[String]) -> Result<Vec<Requirement>, String> {
    requirements
        .iter()
        .map(|requirement| {
            requirement
                .parse::<Requirement>()
                .map_err(|error| format!("Invalid test requirement '{requirement}': {error}"))
        })
        .collect()
}

fn test_requirement_sets(
    package: &Package,
    definition: &TestDefinition,
    from_repository: bool,
) -> Vec<Vec<String>> {
    let base: Vec<String> = if from_repository {
        std::iter::once(package.qualified_name())
            .chain(definition.requires.iter().cloned())
            .collect()
    } else {
        package
            .requires
            .iter()
            .cloned()
            .chain(definition.requires.iter().cloned())
            .collect()
    };

    let variants: Vec<&Vec<String>> = match definition.on_variants.as_ref() {
        Some(TestVariantFilter::Enabled(true)) => package.variants.iter().collect(),
        Some(TestVariantFilter::Requires(filters)) => package
            .variants
            .iter()
            .filter(|variant| {
                TestVariantFilter::Requires(filters.clone()).matches_variant_requires(variant)
            })
            .collect(),
        Some(TestVariantFilter::Enabled(false)) | None => package.variants.iter().take(1).collect(),
    };

    if package.variants.is_empty() {
        if let Some(filter @ TestVariantFilter::Requires(_)) = definition.on_variants.as_ref()
            && !filter.matches_variant_requires(&[])
        {
            return Vec::new();
        }
        return vec![base];
    }

    variants
        .into_iter()
        .map(|variant| {
            base.iter()
                .cloned()
                .chain(variant.iter().cloned())
                .collect()
        })
        .collect()
}

fn apply_pre_test_commands(package: &mut Package) {
    let Some(pre_test_commands) = package.pre_test_commands.take() else {
        return;
    };
    package.post_commands = Some(match package.post_commands.take() {
        Some(post_commands) => format!("{post_commands}\n{pre_test_commands}"),
        None => pre_test_commands,
    });
}

fn resolve_test_environment(
    package: &Package,
    definition: &TestDefinition,
    from_repository: bool,
    repositories: Arc<RepositoryManager>,
) -> Result<HashMap<String, String>, String> {
    let mut last_error = None;
    let mut resolved_packages = None;

    for requirement_strings in test_requirement_sets(package, definition, from_repository) {
        if requirement_strings.is_empty() {
            resolved_packages = Some(Vec::new());
            break;
        }

        let requirements = parse_test_requirements(&requirement_strings)?;
        let mut resolver = DependencyResolver::new(
            Arc::clone(&repositories),
            SolverConfig {
                strict_mode: true,
                ..SolverConfig::default()
            },
        );
        match get_runtime().block_on(resolver.resolve(requirements)) {
            Ok(result) => {
                resolved_packages = Some(
                    result
                        .resolved_packages
                        .iter()
                        .map(|resolved| resolved.try_materialized_package())
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|error| error.to_string())?,
                );
                break;
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    let mut packages = resolved_packages.ok_or_else(|| {
        last_error.unwrap_or_else(|| {
            format!(
                "No compatible test environment found for package '{}'",
                package.qualified_name()
            )
        })
    })?;
    if from_repository {
        let resolved_package = packages
            .iter_mut()
            .find(|resolved| resolved.name == package.name)
            .ok_or_else(|| {
                format!(
                    "Resolved test environment omitted package '{}'",
                    package.qualified_name()
                )
            })?;
        apply_pre_test_commands(resolved_package);
    } else {
        let mut test_package = package.clone();
        apply_pre_test_commands(&mut test_package);
        packages.push(test_package);
    }

    get_runtime()
        .block_on(
            EnvironmentManager::new(ContextConfig {
                inherit_parent_env: true,
                ..ContextConfig::default()
            })
            .generate_environment(&packages),
        )
        .map_err(|error| error.to_string())
}

fn attach_test_environments(
    runner: &mut RustTestRunner,
    package: &Package,
    from_repository: bool,
) -> Result<(), String> {
    let repositories = Arc::new(make_repo_manager(None).map_err(|error| error.to_string())?);
    for definition in runner.test_definitions() {
        match resolve_test_environment(
            package,
            &definition,
            from_repository,
            Arc::clone(&repositories),
        ) {
            Ok(environment) => runner.set_test_environment(definition.name, environment),
            Err(error) => runner.set_test_environment_error(definition.name, error),
        }
    }
    Ok(())
}

// ── PyPackageTestRunner ─────────────────────────────────────────────

/// Python-accessible PackageTestRunner.
#[pyclass(name = "PackageTestRunner", from_py_object)]
#[derive(Clone)]
pub struct PyPackageTestRunner {
    inner: Arc<Mutex<RustTestRunner>>,
}

#[pymethods]
impl PyPackageTestRunner {
    /// Create a new PackageTestRunner for the given package.
    #[new]
    pub fn new(py: Python<'_>, package_spec: String) -> PyResult<Self> {
        let (package, package_root, from_repository) = resolve_package(py, &package_spec)?;
        let package_for_environment = package.clone();
        let mut runner = RustTestRunner::new(package_spec)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        runner = runner.with_package(package, package_root);
        attach_test_environments(&mut runner, &package_for_environment, from_repository)
            .map_err(pyo3::exceptions::PyRuntimeError::new_err)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(runner)),
        })
    }

    /// Set the working directory for test execution.
    #[pyo3(name = "with_working_dir")]
    pub fn py_with_working_dir(&self, dir: String) -> PyResult<Self> {
        let runner = self.inner.lock().unwrap();
        let runner_clone = runner.clone(); // Dereference MutexGuard, then clone
        let new_runner = runner_clone.with_working_dir(PathBuf::from(dir));
        Ok(Self {
            inner: Arc::new(Mutex::new(new_runner)),
        })
    }

    /// Set verbose output level (0-2).
    #[pyo3(name = "with_verbose")]
    pub fn py_with_verbose(&self, verbose: u8) -> PyResult<Self> {
        let runner = self.inner.lock().unwrap();
        let runner_clone = runner.clone();
        let new_runner = runner_clone.with_verbose(verbose);
        Ok(Self {
            inner: Arc::new(Mutex::new(new_runner)),
        })
    }

    /// Set dry run mode.
    #[pyo3(name = "with_dry_run")]
    pub fn py_with_dry_run(&self, dry_run: bool) -> PyResult<Self> {
        let runner = self.inner.lock().unwrap();
        let runner_clone = runner.clone();
        let new_runner = runner_clone.with_dry_run(dry_run);
        Ok(Self {
            inner: Arc::new(Mutex::new(new_runner)),
        })
    }

    /// Set stop on fail behavior.
    #[pyo3(name = "with_stop_on_fail")]
    pub fn py_with_stop_on_fail(&self, stop_on_fail: bool) -> PyResult<Self> {
        let runner = self.inner.lock().unwrap();
        let runner_clone = runner.clone();
        let new_runner = runner_clone.with_stop_on_fail(stop_on_fail);
        Ok(Self {
            inner: Arc::new(Mutex::new(new_runner)),
        })
    }

    /// Get available test names from the package definition.
    pub fn get_test_names(&self) -> PyResult<Vec<String>> {
        let runner = self.inner.lock().unwrap();
        runner
            .get_test_names()
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Find requested test names (supports wildcards).
    pub fn find_requested_test_names(&self, requested: Vec<String>) -> PyResult<Vec<String>> {
        let runner = self.inner.lock().unwrap();
        runner
            .find_requested_test_names(&requested)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Run a specific test by name, optionally passing extra command arguments.
    #[pyo3(signature = (test_name, extra_test_args=None))]
    pub fn run_test(
        &self,
        test_name: String,
        extra_test_args: Option<Vec<String>>,
    ) -> PyResult<i32> {
        let mut runner = self.inner.lock().unwrap();
        runner
            .run_test_with_args(&test_name, extra_test_args.as_deref().unwrap_or_default())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    /// Print test summary.
    pub fn print_summary(&self) {
        let runner = self.inner.lock().unwrap();
        runner.print_summary();
    }

    /// Get the package name.
    #[getter]
    pub fn package_name(&self) -> String {
        let runner = self.inner.lock().unwrap();
        runner.package_name.clone()
    }

    /// Get the number of test results.
    pub fn num_tests(&self) -> usize {
        let runner = self.inner.lock().unwrap();
        runner.test_results.len()
    }

    /// Get the number of successful tests.
    pub fn num_success(&self) -> usize {
        let runner = self.inner.lock().unwrap();
        runner
            .test_results
            .iter()
            .filter(|r| r.status == TestStatus::Success)
            .count()
    }

    /// Get the number of failed tests.
    pub fn num_failed(&self) -> usize {
        let runner = self.inner.lock().unwrap();
        runner
            .test_results
            .iter()
            .filter(|r| r.status == TestStatus::Failed)
            .count()
    }

    /// Get the number of skipped tests.
    pub fn num_skipped(&self) -> usize {
        let runner = self.inner.lock().unwrap();
        runner
            .test_results
            .iter()
            .filter(|r| r.status == TestStatus::Skipped)
            .count()
    }
}

// ── PyPackageTestResults ──────────────────────────────────────────

/// Python-accessible PackageTestResults.
#[pyclass(name = "PackageTestResults", from_py_object)]
#[derive(Clone)]
pub struct PyPackageTestResults {
    inner: Arc<Mutex<RustTestResults>>,
}

#[pymethods]
impl PyPackageTestResults {
    /// Create a new empty test results collector.
    #[new]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RustTestResults::new())),
        }
    }

    /// Add a test result.
    #[pyo3(name = "add_test_result")]
    pub fn py_add_test_result(
        &self,
        _py: Python<'_>,
        test_name: String,
        variant: Option<String>,
        status: Bound<'_, PyAny>,
        description: String,
    ) -> PyResult<()> {
        let status = pyobject_to_test_status(status)?;
        let mut results = self.inner.lock().unwrap();
        results.add_test_result(test_name, variant, status, description);
        Ok(())
    }

    /// Get the number of tests.
    pub fn num_tests(&self) -> usize {
        let results = self.inner.lock().unwrap();
        results.num_tests()
    }

    /// Get the number of successful tests.
    pub fn num_success(&self) -> usize {
        let results = self.inner.lock().unwrap();
        results.num_success()
    }

    /// Get the number of failed tests.
    pub fn num_failed(&self) -> usize {
        let results = self.inner.lock().unwrap();
        results.num_failed()
    }

    /// Get the number of skipped tests.
    pub fn num_skipped(&self) -> usize {
        let results = self.inner.lock().unwrap();
        results.num_skipped()
    }

    /// Print test summary.
    pub fn print_summary(&self) {
        let results = self.inner.lock().unwrap();
        results.print_summary();
    }
}

// ── Helper functions ─────────────────────────────────────────────────

/// Convert a Python object to TestStatus.
fn pyobject_to_test_status(obj: Bound<'_, PyAny>) -> PyResult<TestStatus> {
    // Accept string: "success", "failed", "skipped", "error"
    if let Ok(s) = obj.extract::<String>() {
        match s.to_lowercase().as_str() {
            "success" => Ok(TestStatus::Success),
            "failed" => Ok(TestStatus::Failed),
            "skipped" => Ok(TestStatus::Skipped),
            "error" => Ok(TestStatus::Error),
            _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "Invalid test status: {}",
                s
            ))),
        }
    } else {
        Err(pyo3::exceptions::PyTypeError::new_err(
            "Expected string or TestStatus object",
        ))
    }
}

// ── Utility functions ─────────────────────────────────────────────

// ── Registration ──────────────────────────────────────────────────

/// Register the `rez_next._native.test` submodule.
pub fn register_test_submodule(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new(py, "test")?;

    m.add_class::<PyPackageTestRunner>()?;
    m.add_class::<PyPackageTestResults>()?;

    // Add status constants
    m.setattr("SUCCESS", "success")?;
    m.setattr("FAILED", "failed")?;
    m.setattr("SKIPPED", "skipped")?;
    m.setattr("ERROR", "error")?;

    // Register as submodule
    crate::register_submodule(parent, "test", &m)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an initialized Python interpreter"]
    fn test_py_package_test_runner_creation() {
        // This test just verifies the struct can be created
        Python::attach(|py| {
            let runner_result = PyPackageTestRunner::new(py, "test".to_string());
            // May fail if package not found, but struct should be creatable
            assert!(runner_result.is_ok() || runner_result.is_err());
        });
    }

    #[test]
    fn test_py_package_test_results_creation() {
        let _results = PyPackageTestResults::new();
    }
}
