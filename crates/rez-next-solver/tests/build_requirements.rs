use std::fs;
use std::path::Path;
use std::sync::Arc;

use rez_next_package::Requirement;
use rez_next_repository::simple_repository::{RepositoryManager, SimpleRepository};
use rez_next_solver::{DependencyResolver, SolverConfig};

#[test]
fn build_resolution_propagates_public_requirements_without_private_leakage() {
    let temporary = tempfile::tempdir().unwrap();
    write_package(
        temporary.path(),
        "public_dependency",
        "requires = []\nbuild_requires = ['public_tool']\n",
    );
    write_package(
        temporary.path(),
        "public_tool",
        "requires = []\nbuild_requires = ['public_chain_tool']\n",
    );
    write_package(temporary.path(), "public_chain_tool", "");
    write_package(
        temporary.path(),
        "private_dependency",
        "requires = []\nprivate_build_requires = ['private_tool']\n",
    );
    write_package(temporary.path(), "private_tool", "");
    write_package(temporary.path(), "root_public_tool", "");
    write_package(temporary.path(), "root_private_tool", "");

    let mut repositories = RepositoryManager::new();
    repositories.add_repository(Box::new(SimpleRepository::new(
        temporary.path(),
        "test".to_string(),
    )));
    let mut resolver = DependencyResolver::new(
        Arc::new(repositories),
        SolverConfig {
            strict_mode: true,
            ..SolverConfig::default()
        },
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let runtime_requirements = parse_requirements(&["public_dependency", "private_dependency"]);
    let runtime_result = runtime
        .block_on(resolver.resolve(runtime_requirements))
        .unwrap();
    let runtime_names = package_names(&runtime_result.resolved_packages);
    assert!(runtime_names.contains(&"public_dependency"));
    assert!(runtime_names.contains(&"private_dependency"));
    assert!(!runtime_names.contains(&"public_tool"));
    assert!(!runtime_names.contains(&"private_tool"));

    let build_requirements = parse_requirements(&[
        "public_dependency",
        "private_dependency",
        "root_public_tool",
        "root_private_tool",
    ]);
    let build_result = runtime
        .block_on(resolver.resolve_build(build_requirements))
        .unwrap();
    let build_names = package_names(&build_result.resolved_packages);
    for expected in [
        "public_dependency",
        "private_dependency",
        "public_tool",
        "public_chain_tool",
        "root_public_tool",
        "root_private_tool",
    ] {
        assert!(
            build_names.contains(&expected),
            "build resolution should include {expected}, got {build_names:?}"
        );
    }
    assert!(
        !build_names.contains(&"private_tool"),
        "private build requirements from dependencies must not propagate: {build_names:?}"
    );
}

fn parse_requirements(names: &[&str]) -> Vec<Requirement> {
    names.iter().map(|name| name.parse().unwrap()).collect()
}

fn package_names(packages: &[rez_next_solver::ResolvedPackageInfo]) -> Vec<&str> {
    packages
        .iter()
        .map(|package| package.package.name.as_str())
        .collect()
}

fn write_package(repository: &Path, name: &str, fields: &str) {
    let package_directory = repository.join(name).join("1.0.0");
    fs::create_dir_all(&package_directory).unwrap();
    fs::write(
        package_directory.join("package.py"),
        format!("name = '{name}'\nversion = '1.0.0'\n{fields}"),
    )
    .unwrap();
}
