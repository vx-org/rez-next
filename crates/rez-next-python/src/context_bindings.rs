//! Python bindings for ResolvedContext

use crate::package_bindings::{PyPackage, PyPackageRequirement};
use crate::package_functions::expand_home;
use crate::runtime::get_runtime;
use crate::shell_utils::shell_type_from_str;
use crate::source_bindings::detect_current_shell;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use rez_next_context::{ContextStatus, ResolvedContext};
use rez_next_package::{PackageRequirement, Requirement};
use rez_next_repository::simple_repository::{RepositoryManager, SimpleRepository};
use rez_next_solver::{DependencyResolver, SolverConfig};
use std::path::PathBuf;
use std::sync::Arc;

fn configured_implicit_packages(
    py: Python<'_>,
    python_config: Option<&Bound<'_, PyAny>>,
) -> PyResult<Vec<PackageRequirement>> {
    let Some(raw_requirements) = python_config
        .and_then(|config| config.getattr("implicit_packages").ok())
        .and_then(|value| value.extract::<Vec<String>>().ok())
    else {
        return Ok(Vec::new());
    };

    let system = py.import("rez_next.system")?.getattr("system")?;
    let platform: String = system.getattr("platform")?.extract()?;
    let arch: String = system.getattr("arch")?.extract()?;
    let os: String = system.getattr("os")?.extract()?;

    raw_requirements
        .into_iter()
        .map(|requirement| {
            let expanded = requirement
                .replace("{system.platform}", &platform)
                .replace("{system.arch}", &arch)
                .replace("{system.os}", &os);
            PackageRequirement::parse(&expanded)
                .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))
        })
        .collect()
}

/// Python-accessible ResolvedContext class, compatible with rez.resolved_context.ResolvedContext
#[pyclass(name = "ResolvedContext")]
pub struct PyResolvedContext {
    inner: ResolvedContext,
    /// Paths used for resolution
    paths: Vec<PathBuf>,
}

#[pymethods]
impl PyResolvedContext {
    /// Create a new ResolvedContext by resolving the given package requirements.
    /// Compatible with `rez.ResolvedContext(["python-3.9", "maya-2024"])`
    #[new]
    #[pyo3(signature = (packages, paths=None, add_implicit_packages=true))]
    pub fn new(
        py: Python<'_>,
        packages: Vec<String>,
        paths: Option<Vec<String>>,
        add_implicit_packages: bool,
    ) -> PyResult<Self> {
        use rez_next_common::config::RezCoreConfig;

        let config = RezCoreConfig::load();
        let mut parent_variables = config.parent_variables.clone();
        let mut all_parent_variables = config.all_parent_variables;
        let python_config = py
            .import("rez_next.config")
            .and_then(|module| module.getattr("config"))
            .ok();
        if let Some(python_config) = &python_config {
            if let Ok(value) = python_config
                .getattr("parent_variables")
                .and_then(|value| value.extract::<Vec<String>>())
            {
                parent_variables = value;
            }
            if let Ok(value) = python_config
                .getattr("all_parent_variables")
                .and_then(|value| value.extract::<bool>())
            {
                all_parent_variables = value;
            }
        }

        let pkg_paths: Vec<PathBuf> = paths
            .as_ref()
            .map(|p| p.iter().map(PathBuf::from).collect())
            .unwrap_or_else(|| {
                config
                    .packages_path
                    .iter()
                    .map(|p| PathBuf::from(expand_home(p)))
                    .collect()
            });

        // Parse requirements
        let requirements: Vec<PackageRequirement> = packages
            .iter()
            .map(|s| PackageRequirement::parse(s))
            .collect::<Result<_, _>>()
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let implicit_packages = if add_implicit_packages {
            configured_implicit_packages(py, python_config.as_ref())?
        } else {
            Vec::new()
        };

        // Build repository manager
        let mut repo_manager = RepositoryManager::new();
        for (i, path) in pkg_paths.iter().enumerate() {
            if path.exists() {
                repo_manager.add_repository(Box::new(SimpleRepository::new(
                    path.clone(),
                    format!("repo_{}", i),
                )));
            }
        }

        // Convert to Requirement for the resolver via string parsing
        let resolver_reqs: Vec<Requirement> = requirements
            .iter()
            .chain(&implicit_packages)
            .map(|pr| {
                let req_str = pr.to_string();
                req_str
                    .parse::<Requirement>()
                    .unwrap_or_else(|_| Requirement::new(pr.name.clone()))
            })
            .collect();

        let rt = get_runtime();

        let repo_arc = Arc::new(repo_manager);
        let mut resolver = DependencyResolver::new(Arc::clone(&repo_arc), SolverConfig::default());
        let resolution = rt
            .block_on(resolver.resolve(resolver_reqs))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        let mut context = ResolvedContext::from_requirements(requirements);
        context.implicit_packages = implicit_packages;
        context.resolved_packages = resolution
            .resolved_packages
            .into_iter()
            .map(|info| info.try_materialized_package())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;
        context.resolved_ephemerals = resolution
            .resolved_ephemerals
            .iter()
            .map(rez_next_solver::ResolvedEphemeral::as_package_requirement)
            .collect();
        context.config.parent_variables = parent_variables;
        context.config.all_parent_variables = all_parent_variables;
        let encoded_package_paths = std::env::join_paths(&pkg_paths)
            .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?
            .to_string_lossy()
            .into_owned();
        context
            .metadata
            .insert("package_paths".to_string(), encoded_package_paths);
        context.status = ContextStatus::Resolved;

        Ok(PyResolvedContext {
            inner: context,
            paths: pkg_paths,
        })
    }

    fn __str__(&self) -> String {
        format!(
            "ResolvedContext({} packages)",
            self.inner.resolved_packages.len()
        )
    }

    fn __repr__(&self) -> String {
        format!(
            "ResolvedContext(packages={:?}, paths={})",
            self.inner
                .resolved_packages
                .iter()
                .map(|p| p.name.clone())
                .collect::<Vec<_>>(),
            self.paths.len()
        )
    }

    /// Whether the context resolved successfully
    #[getter]
    fn success(&self) -> bool {
        self.inner.status == ContextStatus::Resolved
    }

    /// List of resolved packages
    #[getter]
    fn resolved_packages(&self) -> Vec<PyPackage> {
        self.inner
            .resolved_packages
            .iter()
            .map(|p| PyPackage(p.clone()))
            .collect()
    }

    /// Package search paths used to resolve this context.
    #[getter]
    fn package_paths(&self) -> Vec<String> {
        self.paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    /// Number of resolved packages
    #[getter]
    fn num_resolved_packages(&self) -> usize {
        self.inner.resolved_packages.len()
    }

    /// Get a resolved package by name (or None)
    fn get_resolved_package(&self, name: &str) -> Option<PyPackage> {
        self.inner
            .resolved_packages
            .iter()
            .find(|p| p.name == name)
            .map(|p| PyPackage(p.clone()))
    }

    /// Get environment variables for this context (as dict).
    ///
    /// `parent_environ` matches Rez's public API and is interpreted according
    /// to `parent_variables` / `all_parent_variables`.
    #[pyo3(signature = (parent_environ=None))]
    fn get_environ(
        &self,
        py: Python,
        parent_environ: Option<std::collections::HashMap<String, String>>,
    ) -> PyResult<Py<PyAny>> {
        let rt = get_runtime();

        let env_vars = rt
            .block_on(self.inner.get_environ(parent_environ))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        let dict = PyDict::new(py);
        for (k, v) in env_vars {
            dict.set_item(k, v)?;
        }
        Ok(dict.into_any().unbind())
    }

    /// Apply environment to current process
    fn apply_to_os_environ(&self, py: Python) -> PyResult<()> {
        let rt = get_runtime();

        let env_vars = rt
            .block_on(self.inner.get_environ(None))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        apply_python_environment(py, env_vars)
    }

    /// Return the package requests used to create this context.
    #[pyo3(signature = (include_implicit=false))]
    fn requested_packages(&self, include_implicit: bool) -> Vec<PyPackageRequirement> {
        let explicit = self
            .inner
            .requirements
            .iter()
            .cloned()
            .map(PyPackageRequirement);
        if include_implicit {
            explicit
                .chain(
                    self.inner
                        .implicit_packages
                        .iter()
                        .cloned()
                        .map(PyPackageRequirement),
                )
                .collect()
        } else {
            explicit.collect()
        }
    }

    /// Get final non-conflict ephemeral requests.
    #[getter]
    fn resolved_ephemerals(&self) -> Vec<PyPackageRequirement> {
        self.inner
            .resolved_ephemerals
            .iter()
            .cloned()
            .map(PyPackageRequirement)
            .collect()
    }

    /// Apply the context to the current Python process environment.
    #[pyo3(signature = (parent_environ=None))]
    fn apply(
        &self,
        py: Python,
        parent_environ: Option<std::collections::HashMap<String, String>>,
    ) -> PyResult<()> {
        let env_vars = get_runtime()
            .block_on(self.inner.get_environ(parent_environ))
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;

        apply_python_environment(py, env_vars)
    }

    /// Execute a command in the resolved context and return subprocess.Popen.
    #[pyo3(signature = (args, parent_environ=None, **popen_args))]
    fn execute_command(
        &self,
        py: Python<'_>,
        args: Bound<'_, PyAny>,
        parent_environ: Option<std::collections::HashMap<String, String>>,
        popen_args: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        let generated_environment = get_runtime()
            .block_on(self.inner.get_environ(parent_environ.clone()))
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;

        let process_environment = PyDict::new(py);
        if let Some(parent_environment) = parent_environ {
            for (name, value) in parent_environment {
                process_environment.set_item(name, value)?;
            }
        } else {
            let os_environment = py.import("os")?.getattr("environ")?;
            for item in os_environment.call_method0("items")?.try_iter()? {
                let (name, value): (String, String) = item?.extract()?;
                process_environment.set_item(name, value)?;
            }
        }
        for (name, value) in generated_environment {
            process_environment.set_item(name, value)?;
        }

        let keyword_arguments = PyDict::new(py);
        if let Some(popen_args) = popen_args {
            for (key, value) in popen_args.iter() {
                keyword_arguments.set_item(key, value)?;
            }
        }
        keyword_arguments.set_item("env", process_environment)?;

        let process = py
            .import("subprocess")?
            .getattr("Popen")?
            .call((args,), Some(&keyword_arguments))?;
        Ok(process.unbind())
    }

    /// Context ID
    #[getter]
    fn id(&self) -> String {
        self.inner.id.clone()
    }

    /// Creation timestamp
    #[getter]
    fn created_at(&self) -> i64 {
        self.inner.created_at
    }

    /// Save context to file (.rxt format)
    fn save(&self, path: &str) -> PyResult<()> {
        use rez_next_context::{ContextFormat, ContextSerializer};
        use std::path::Path;

        let rt = get_runtime();

        rt.block_on(ContextSerializer::save_to_file(
            &self.inner,
            Path::new(path),
            ContextFormat::Json,
        ))
        .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))
    }

    /// Load context from file
    #[staticmethod]
    fn load(path: &str) -> PyResult<PyResolvedContext> {
        use rez_next_context::ContextSerializer;
        use std::path::Path;

        let rt = get_runtime();

        let context = rt
            .block_on(ContextSerializer::load_from_file(Path::new(path)))
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;

        Ok(PyResolvedContext {
            inner: context,
            paths: Vec::new(),
        })
    }

    /// Print the context summary (rez compat: context.print_info())
    fn print_info(&self) -> String {
        let summary = self.inner.get_summary();
        let mut out = format!("resolved packages ({}):\n", summary.package_count);
        for (name, version) in &summary.package_versions {
            out.push_str(&format!("  {}-{}\n", name, version));
        }
        out
    }

    /// Generate a shell activation script for the resolved context.
    /// Compatible with `context.get_shell_code(shell)`.
    /// shell: "bash" | "zsh" | "fish" | "cmd" | "powershell" (default: auto-detect)
    #[pyo3(signature = (shell=None))]
    fn to_shell_script(&self, shell: Option<&str>) -> PyResult<String> {
        use rez_next_rex::generate_shell_script;

        let shell_name = shell
            .map(|s| s.to_string())
            .unwrap_or_else(detect_current_shell);
        let shell_type = shell_type_from_str(&shell_name);

        let rt = get_runtime();

        let rex_env = rt
            .block_on(self.inner.get_rex_environment(None))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

        Ok(generate_shell_script(&rex_env, &shell_type))
    }

    /// Generate shell activation code using Rez's public method name.
    #[pyo3(signature = (shell=None, parent_environ=None, style=None))]
    fn get_shell_code(
        &self,
        shell: Option<&str>,
        parent_environ: Option<std::collections::HashMap<String, String>>,
        style: Option<Py<PyAny>>,
    ) -> PyResult<String> {
        use rez_next_rex::generate_shell_script;

        let _ = style;
        let shell_name = shell
            .map(str::to_string)
            .unwrap_or_else(detect_current_shell);
        let shell_type = shell_type_from_str(&shell_name);
        let rex_environment = get_runtime()
            .block_on(self.inner.get_rex_environment(parent_environ))
            .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(error.to_string()))?;

        Ok(generate_shell_script(&rex_environment, &shell_type))
    }

    /// Return `{package_name: (package, [tools])}` using Rez's public shape.
    #[pyo3(signature = (request_only=false))]
    fn get_tools(&self, py: Python, request_only: bool) -> PyResult<Py<PyAny>> {
        let requested_names = self
            .inner
            .requirements
            .iter()
            .filter(|requirement| !requirement.conflict)
            .map(|requirement| requirement.name.as_str())
            .collect::<std::collections::HashSet<_>>();
        let dict = PyDict::new(py);
        for pkg in &self.inner.resolved_packages {
            if pkg.tools.is_empty()
                || (request_only && !requested_names.contains(pkg.name.as_str()))
            {
                continue;
            }
            let package = Py::new(py, PyPackage(pkg.clone()))?;
            dict.set_item(&pkg.name, (package, pkg.tools.clone()))?;
        }
        Ok(dict.into_any().unbind())
    }

    /// Get the context as a dict (rez compat: for serialization)
    fn to_dict(&self, py: Python) -> PyResult<Py<PyAny>> {
        let dict = PyDict::new(py);
        dict.set_item("id", &self.inner.id)?;
        dict.set_item("status", format!("{:?}", self.inner.status))?;
        dict.set_item(
            "packages",
            self.inner
                .resolved_packages
                .iter()
                .map(|p| {
                    format!(
                        "{}-{}",
                        p.name,
                        p.version.as_ref().map(|v| v.as_str()).unwrap_or("unknown")
                    )
                })
                .collect::<Vec<_>>(),
        )?;
        dict.set_item("num_packages", self.inner.resolved_packages.len())?;
        Ok(dict.into_any().unbind())
    }

    /// Check if this context failed to resolve.
    /// Compatible with `context.failure_description` (returns None if success).
    #[getter]
    fn failure_description(&self) -> Option<String> {
        if self.inner.status == ContextStatus::Failed {
            Some("Resolution failed".to_string())
        } else {
            None
        }
    }

    /// Get the resolved packages as a list of (name, version) tuples.
    fn get_resolved_packages_info(&self, py: Python) -> PyResult<Py<PyAny>> {
        use pyo3::types::PyList;
        let list = PyList::empty(py);
        for pkg in &self.inner.resolved_packages {
            let ver = pkg
                .version
                .as_ref()
                .map(|v| v.as_str())
                .unwrap_or("unknown");
            let tuple = pyo3::types::PyTuple::new(
                py,
                [
                    pkg.name.clone().into_pyobject(py)?.into_any().unbind(),
                    ver.to_string().into_pyobject(py)?.into_any().unbind(),
                ],
            )?;
            list.append(tuple)?;
        }
        Ok(list.into_any().unbind())
    }
}

fn apply_python_environment(
    py: Python,
    environment: std::collections::HashMap<String, String>,
) -> PyResult<()> {
    let os_environment = py.import("os")?.getattr("environ")?;
    for (name, value) in environment {
        os_environment.set_item(name, value)?;
    }
    Ok(())
}

// ─── Rust unit tests ─────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "context_bindings_tests.rs"]
mod context_bindings_tests;
