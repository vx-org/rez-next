//! Direct benchmarks for the solver's heuristic cost functions.

use criterion::{Criterion, criterion_group, criterion_main};
use rez_next_package::{Package, PackageRequirement};
use rez_next_solver::{
    AStarConflictType, AStarDependencyConflict, CompositeHeuristic, DependencyHeuristic,
    HeuristicConfig, HeuristicFactory, SearchState,
};
use rez_next_version::Version;
use std::hint::black_box;

#[path = "support/package_criterion.rs"]
mod package_criterion;

fn make_state() -> SearchState {
    let requirements = (0..24)
        .map(|index| PackageRequirement::new(format!("tool_{index}")))
        .collect();
    let mut state = SearchState::new_initial(requirements);
    for index in 0..16 {
        let mut package = Package::new(format!("resolved_{index}"));
        package.set_version(Version::parse("1.2.3").expect("valid benchmark version"));
        state
            .resolved_packages
            .insert(package.name.clone(), package);
    }
    state.add_conflict(AStarDependencyConflict::new(
        "tool_0".to_string(),
        vec!["tool_0-1".to_string(), "tool_0-2".to_string()],
        0.5,
        AStarConflictType::VersionConflict,
    ));
    state
}

fn bench_heuristics(c: &mut Criterion) {
    let state = make_state();
    let mut group = c.benchmark_group("solver_heuristics");
    let heuristics: [(&str, Box<dyn DependencyHeuristic + Send + Sync>); 5] = [
        (
            "composite_default",
            Box::new(CompositeHeuristic::new(HeuristicConfig::default())),
        ),
        ("composite_fast", Box::new(CompositeHeuristic::new_fast())),
        (
            "composite_thorough",
            Box::new(CompositeHeuristic::new_thorough()),
        ),
        (
            "factory_complex",
            HeuristicFactory::create_for_complexity(64),
        ),
        (
            "factory_conflict_heavy",
            HeuristicFactory::create_for_scenario("conflict_heavy"),
        ),
    ];

    for (name, heuristic) in heuristics {
        group.bench_function(name, |benchmark| {
            benchmark.iter(|| black_box(heuristic.calculate(black_box(&state))))
        });
    }
    group.finish();
}

criterion_group!(
    name = heuristics;
    config = package_criterion::configured();
    targets = bench_heuristics
);
criterion_main!(heuristics);
