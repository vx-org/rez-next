use std::fs;
use std::sync::Arc;

use rez_next_package::Requirement;
use rez_next_repository::simple_repository::{RepositoryManager, SimpleRepository};
use rez_next_solver::{DependencyResolver, SolverConfig};

#[test]
fn weak_implicit_request_constrains_a_strong_package_family() {
    let temporary = tempfile::tempdir().unwrap();
    write_package(temporary.path(), "platform", "linux");
    write_package(temporary.path(), "platform", "windows");

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

    let result = runtime
        .block_on(resolver.resolve(vec![
            Requirement::new("platform".to_string()),
            "~platform==windows".parse().unwrap(),
        ]))
        .unwrap();

    assert_eq!(result.resolved_packages.len(), 1);
    assert_eq!(
        result.resolved_packages[0]
            .package
            .version
            .as_ref()
            .unwrap()
            .as_str(),
        "windows"
    );
}

fn write_package(repository: &std::path::Path, name: &str, version: &str) {
    let package_directory = repository.join(name).join(version);
    fs::create_dir_all(&package_directory).unwrap();
    fs::write(
        package_directory.join("package.py"),
        format!("name = \"{name}\"\nversion = \"{version}\"\n"),
    )
    .unwrap();
}
