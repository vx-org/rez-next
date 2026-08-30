//! Python bindings for rez system information
//!
//! Provides `rez.system` compatible API: platform, arch, os, rez_version, etc.

use pyo3::prelude::*;
use rez_next_common::SystemInfo;

/// System information class - compatible with rez.system
#[pyclass(name = "System")]
pub struct PySystem;

impl Default for PySystem {
    fn default() -> Self {
        Self::new()
    }
}

#[pymethods]
impl PySystem {
    #[new]
    pub fn new() -> Self {
        PySystem
    }

    fn __repr__(&self) -> String {
        format!(
            "System(platform={}, arch={}, os={})",
            Self::platform_str(),
            Self::arch_str(),
            Self::os_str()
        )
    }

    /// Current platform: linux, windows, osx
    #[getter]
    fn platform(&self) -> String {
        Self::platform_str()
    }

    /// Current CPU architecture
    #[getter]
    fn arch(&self) -> String {
        Self::arch_str()
    }

    /// Detailed OS name
    #[getter]
    fn os(&self) -> String {
        Self::os_str()
    }

    /// rez-next version
    #[getter]
    fn rez_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// Python version (of the current interpreter)
    #[getter]
    fn python_version(&self, _py: Python) -> String {
        Python::version_str().to_string()
    }

    /// Number of logical CPUs
    #[getter]
    fn num_cpus(&self) -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }

    /// Hostname
    #[getter]
    fn hostname(&self) -> String {
        std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

impl PySystem {
    pub fn platform_pub() -> String {
        Self::platform_str()
    }

    pub fn arch_pub() -> String {
        Self::arch_str()
    }

    pub fn os_pub() -> String {
        Self::os_str()
    }

    fn platform_str() -> String {
        SystemInfo::detect().platform
    }

    fn arch_str() -> String {
        SystemInfo::detect().arch
    }

    fn os_str() -> String {
        SystemInfo::detect().os
    }
}

/// Global system singleton factory
#[pyfunction]
pub fn get_system() -> PySystem {
    PySystem::new()
}

#[cfg(test)]
#[path = "system_bindings_tests.rs"]
mod tests;
