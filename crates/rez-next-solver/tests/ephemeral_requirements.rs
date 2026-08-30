use std::sync::Arc;

use rez_next_repository::simple_repository::RepositoryManager;
use rez_next_solver::{DependencyResolver, SolverConfig};
use rez_next_version::Version;

#[test]
fn ephemeral_requests_resolve_as_ranges_without_repository_packages() {
    let mut resolver = DependencyResolver::new(
        Arc::new(RepositoryManager::new()),
        SolverConfig {
            strict_mode: true,
            ..SolverConfig::default()
        },
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let result = runtime
        .block_on(resolver.resolve(vec![
            ".feature>=1".parse().unwrap(),
            "!.feature==2".parse().unwrap(),
        ]))
        .unwrap();

    assert!(result.resolved_packages.is_empty());
    assert_eq!(result.resolved_ephemerals.len(), 1);
    let feature = &result.resolved_ephemerals[0];
    assert_eq!(feature.name, ".feature");
    assert!(feature.range.contains(&Version::parse("1").unwrap()));
    assert!(!feature.range.contains(&Version::parse("2").unwrap()));
    assert!(feature.range.contains(&Version::parse("3").unwrap()));
}

#[test]
fn incompatible_ephemeral_ranges_fail_the_resolve() {
    let mut resolver = DependencyResolver::new(
        Arc::new(RepositoryManager::new()),
        SolverConfig {
            strict_mode: true,
            ..SolverConfig::default()
        },
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let error = runtime
        .block_on(resolver.resolve(vec![
            ".feature==1".parse().unwrap(),
            ".feature==2".parse().unwrap(),
        ]))
        .unwrap_err();

    assert!(error.to_string().contains(".feature"));
}
