//! Dependency resolution implementation - equivalent to Python's solver

use crate::SolverConfig;
use crate::resolution_state::ResolutionState;
use rez_next_common::RezCoreError;
use rez_next_package::{Package, PackageInstallLayout, Requirement, VersionConstraint};
use rez_next_repository::simple_repository::RepositoryManager;
use rez_next_version::{Version, VersionRange};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

/// A dependency resolver that finds compatible package combinations
pub struct DependencyResolver {
    /// Repository manager for package discovery
    repository_manager: Arc<RepositoryManager>,

    /// Solver configuration
    config: SolverConfig,

    /// Cache of resolved packages
    package_cache: HashMap<String, Vec<Arc<Package>>>,
}

/// Detailed resolution result containing resolved packages, failures, conflicts and stats
#[derive(Debug, Clone)]
pub struct DetailedResolutionResult {
    /// Successfully resolved packages
    pub resolved_packages: Vec<ResolvedPackageInfo>,

    /// Final non-conflict ephemeral ranges from the request.
    pub resolved_ephemerals: Vec<ResolvedEphemeral>,

    /// Requirements that couldn't be satisfied
    pub failed_requirements: Vec<Requirement>,

    /// Conflicts encountered during resolution
    pub conflicts: Vec<ResolutionConflict>,

    /// Resolution statistics
    pub stats: ResolutionStats,
}

/// A resolved ephemeral feature range, which has no repository package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEphemeral {
    pub name: String,
    pub range: VersionRange,
}

impl ResolvedEphemeral {
    /// Convert to the package-request representation used by resolved contexts.
    #[must_use]
    pub fn as_package_requirement(&self) -> rez_next_package::PackageRequirement {
        let version_spec = if self.range.is_any() {
            None
        } else {
            Some(self.range.as_str().to_string())
        };
        rez_next_package::PackageRequirement {
            name: self.name.clone(),
            version_spec,
            weak: false,
            conflict: false,
        }
    }
}

/// Information about a resolved package
#[derive(Debug, Clone)]
pub struct ResolvedPackageInfo {
    /// The resolved package
    pub package: Arc<Package>,

    /// The variant index if this package has variants
    pub variant_index: Option<usize>,

    /// Whether this package was explicitly requested
    pub requested: bool,

    /// The requirements that led to this package being included
    pub required_by: Vec<String>,

    /// The specific requirement that was satisfied
    pub satisfying_requirement: Option<Requirement>,
}

impl ResolvedPackageInfo {
    /// Return the package descriptor rooted at its selected variant payload.
    #[deprecated(note = "Use try_materialized_package to report invalid installation layouts")]
    pub fn materialized_package(&self) -> Package {
        self.try_materialized_package()
            .expect("resolved package must have a valid canonical installation layout")
    }

    /// Return a descriptor rooted at the canonical selected variant payload.
    /// Invalid selections, unsupported layouts and missing variant bases fail.
    pub fn try_materialized_package(&self) -> Result<Package, RezCoreError> {
        let layout = PackageInstallLayout::for_variant(&self.package, self.variant_index)?;
        let mut package = (*self.package).clone();
        if self.variant_index.is_some() {
            let root = package.root().ok_or_else(|| {
                RezCoreError::PackageParse(format!(
                    "Cannot materialize variant of package '{}' without a definition filepath",
                    package.name
                ))
            })?;
            let definition =
                PathBuf::from(package.filepath.as_ref().expect("root requires filepath"));
            let filename = definition
                .file_name()
                .ok_or_else(|| {
                    RezCoreError::PackageParse(
                        "Cannot materialize a variant without a package definition filename"
                            .to_string(),
                    )
                })?
                .to_os_string();
            let variant_root = PathBuf::from(root).join(layout.variant_subpath());
            package.filepath = Some(variant_root.join(filename).to_string_lossy().into_owned());
        }
        Ok(package)
    }
}

/// A conflict encountered during resolution
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolutionConflict {
    /// The package name that has conflicting requirements
    pub package_name: String,

    /// The conflicting requirements
    pub conflicting_requirements: Vec<Requirement>,

    /// The packages that introduced these requirements
    pub source_packages: Vec<String>,
}

/// Statistics about the resolution process
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct ResolutionStats {
    /// Number of packages considered
    pub packages_considered: usize,

    /// Number of variants evaluated
    pub variants_evaluated: usize,

    /// Time spent resolving (in milliseconds)
    pub resolution_time_ms: u64,

    /// Number of conflicts encountered
    pub conflicts_encountered: usize,

    /// Number of backtracking steps
    pub backtrack_steps: usize,
}

#[derive(Default)]
struct EphemeralRanges {
    positive: Option<VersionRange>,
    excluded: Vec<VersionRange>,
}

fn resolve_ephemerals(
    requirements: Vec<Requirement>,
) -> Result<(Vec<Requirement>, Vec<ResolvedEphemeral>), RezCoreError> {
    let mut regular = Vec::new();
    let mut grouped: BTreeMap<String, EphemeralRanges> = BTreeMap::new();

    for requirement in requirements {
        if !requirement.name.starts_with('.') {
            regular.push(requirement);
            continue;
        }

        let range = requirement_range(&requirement)?;
        let ranges = grouped.entry(requirement.name.clone()).or_default();
        if requirement.conflict {
            ranges.excluded.push(range);
        } else {
            ranges.positive = Some(match ranges.positive.take() {
                Some(current) => current.intersect(&range).ok_or_else(|| {
                    RezCoreError::Solver(format!(
                        "Ephemeral request '{}' has incompatible ranges",
                        requirement.name
                    ))
                })?,
                None => range,
            });
        }
    }

    let mut resolved = Vec::new();
    for (name, ranges) in grouped {
        let Some(mut range) = ranges.positive else {
            continue;
        };
        for excluded in ranges.excluded {
            range = range.subtract(&excluded).ok_or_else(|| {
                RezCoreError::Solver(format!("Ephemeral request '{name}' is fully excluded"))
            })?;
        }
        resolved.push(ResolvedEphemeral { name, range });
    }

    Ok((regular, resolved))
}

fn requirement_range(requirement: &Requirement) -> Result<VersionRange, RezCoreError> {
    let range = match &requirement.version_constraint {
        None | Some(VersionConstraint::Any) => return Ok(VersionRange::any()),
        Some(VersionConstraint::Prefix(version)) => version.as_str().to_string(),
        Some(constraint) => constraint.to_string(),
    };
    VersionRange::parse(&range).map_err(|error| {
        RezCoreError::RequirementParse(format!(
            "Invalid ephemeral request '{}': {error}",
            requirement
        ))
    })
}

impl DependencyResolver {
    /// Create a new dependency resolver
    pub fn new(repository_manager: Arc<RepositoryManager>, config: SolverConfig) -> Self {
        Self {
            repository_manager,
            config,
            package_cache: HashMap::new(),
        }
    }

    /// Resolve a set of requirements into a consistent package set
    pub async fn resolve(
        &mut self,
        requirements: Vec<Requirement>,
    ) -> Result<DetailedResolutionResult, RezCoreError> {
        self.resolve_with_mode(requirements, false).await
    }

    /// Resolve a build environment, propagating public `build_requires`.
    ///
    /// Private build requirements are intentionally absent here. The caller
    /// adds the package currently being built's private requirements to the
    /// root request, while dependencies contribute only their public build
    /// requirements.
    pub async fn resolve_build(
        &mut self,
        requirements: Vec<Requirement>,
    ) -> Result<DetailedResolutionResult, RezCoreError> {
        self.resolve_with_mode(requirements, true).await
    }

    async fn resolve_with_mode(
        &mut self,
        requirements: Vec<Requirement>,
        include_build_requires: bool,
    ) -> Result<DetailedResolutionResult, RezCoreError> {
        let start_time = std::time::Instant::now();

        let (requirements, resolved_ephemerals) = resolve_ephemerals(requirements)?;

        // Initialize resolution state
        let mut resolution_state = ResolutionState::new(requirements.clone());

        // Perform resolution
        let result = self
            .resolve_recursive(&mut resolution_state, include_build_requires)
            .await?;

        // Check for cyclic dependencies after full resolution
        if let Some(cycle) = resolution_state.detect_cycle() {
            return Err(RezCoreError::Solver(format!(
                "Cyclic dependency detected: {}",
                cycle.join(" -> ")
            )));
        }

        // Calculate statistics
        let resolution_time = start_time.elapsed().as_millis() as u64;
        let stats = ResolutionStats {
            packages_considered: resolution_state.packages_considered,
            variants_evaluated: resolution_state.variants_evaluated,
            resolution_time_ms: resolution_time,
            conflicts_encountered: resolution_state.conflicts.len(),
            backtrack_steps: resolution_state.backtrack_steps,
        };

        let failed_requirements = std::mem::take(&mut resolution_state.failed_requirements);
        let mut failed = Vec::new();
        for requirement in failed_requirements {
            if resolution_state.is_requirement_active(&requirement)
                && resolution_state
                    .find_satisfying_package(&requirement)
                    .is_none()
                && !failed.contains(&requirement)
            {
                failed.push(requirement);
            }
        }

        // Strict mode: any unsatisfied requirement is a hard error
        if self.config.strict_mode && !failed.is_empty() {
            let names: Vec<String> = failed.iter().map(|r| r.to_string()).collect();
            return Err(RezCoreError::Solver(format!(
                "Strict mode: failed to satisfy requirements: {}",
                names.join(", ")
            )));
        }

        Ok(DetailedResolutionResult {
            resolved_packages: result,
            resolved_ephemerals,
            failed_requirements: failed,
            conflicts: resolution_state.conflicts,
            stats,
        })
    }

    /// Recursive resolution implementation
    async fn resolve_recursive(
        &mut self,
        state: &mut ResolutionState,
        include_build_requires: bool,
    ) -> Result<Vec<ResolvedPackageInfo>, RezCoreError> {
        // Get next requirement to resolve
        while let Some(requirement) = state.get_next_requirement() {
            // Check if we already have a package that satisfies this requirement
            if let Some(existing) = state.find_satisfying_package(&requirement) {
                // Mark this requirement as satisfied
                state.mark_requirement_satisfied(&requirement, existing.package.name.clone());
                continue;
            }

            // Weak requirements constrain a package when it is otherwise part
            // of the solve, but never introduce that package on their own.
            if requirement.weak
                && !state
                    .resolved_packages
                    .iter()
                    .any(|resolved| resolved.package.name == requirement.name)
            {
                continue;
            }

            // Find candidate packages for this requirement
            let candidates = self.find_candidate_packages(&requirement).await?;
            state.packages_considered += candidates.len();
            let has_resolved_package = state
                .resolved_packages
                .iter()
                .any(|resolved| resolved.package.name == requirement.name);

            if candidates.is_empty() {
                state.failed_requirements.push(requirement.clone());
                continue;
            }

            // Try each candidate
            let mut resolved = false;
            let mut best_effort_candidate = None;
            for candidate in candidates {
                if best_effort_candidate.is_none()
                    && state.check_explicit_conflicts(&candidate).is_none()
                {
                    best_effort_candidate = Some(candidate.clone());
                }
                // Check for conflicts with existing packages
                if let Some(conflict) = state.check_conflicts(&candidate, &requirement) {
                    state.conflicts.push(conflict);
                    continue;
                }

                // Try to resolve with this candidate
                if let Ok((resolved_info, dependencies, conflicts)) = self
                    .try_resolve_with_candidate(
                        state,
                        &candidate,
                        &requirement,
                        include_build_requires,
                    )
                    .await
                {
                    if state.is_resolution_rejected(&resolved_info) {
                        continue;
                    }
                    let package_name = resolved_info.package.name.clone();
                    state.add_resolved_package(resolved_info);
                    state.set_package_requirements(package_name, dependencies, conflicts);
                    resolved = true;
                    break;
                }
            }

            if !resolved
                && !self.config.strict_mode
                && !has_resolved_package
                && let Some(candidate) = best_effort_candidate
                && let Ok((resolved_info, dependencies, conflicts)) = self
                    .try_resolve_with_candidate(
                        state,
                        &candidate,
                        &requirement,
                        include_build_requires,
                    )
                    .await
            {
                let package_name = resolved_info.package.name.clone();
                state.add_resolved_package(resolved_info);
                state.set_package_requirements(package_name, dependencies, conflicts);
                resolved = true;
            }

            if !resolved
                && self.config.strict_mode
                && self
                    .backtrack_dependency_source(state, &requirement, include_build_requires)
                    .await?
            {
                continue;
            }

            if !resolved {
                state.failed_requirements.push(requirement);
            }
        }

        Ok(state.resolved_packages.clone())
    }

    async fn backtrack_dependency_source(
        &mut self,
        state: &mut ResolutionState,
        failed_requirement: &Requirement,
        include_build_requires: bool,
    ) -> Result<bool, RezCoreError> {
        let target_candidates = self.find_candidate_packages(failed_requirement).await?;
        let mut exact_sources = Vec::new();
        let mut incompatible_sources = Vec::new();
        let mut other_sources = Vec::new();

        for resolved in &state.resolved_packages {
            let source = &resolved.package.name;
            let Some(requirements) = state.package_requirements.get(source) else {
                continue;
            };
            let target_requirements: Vec<_> = requirements
                .iter()
                .filter(|requirement| requirement.name == failed_requirement.name)
                .collect();
            if target_requirements.is_empty() {
                continue;
            }

            if target_requirements.contains(&failed_requirement) {
                exact_sources.push(source.clone());
            } else if target_requirements.iter().any(|requirement| {
                !target_candidates.iter().any(|candidate| {
                    candidate
                        .version
                        .as_ref()
                        .is_some_and(|version| requirement.is_satisfied_by(version))
                })
            }) {
                incompatible_sources.push(source.clone());
            } else {
                other_sources.push(source.clone());
            }
        }

        exact_sources.extend(incompatible_sources);
        exact_sources.extend(other_sources);
        for source in exact_sources {
            if self
                .backtrack_package(state, &source, include_build_requires)
                .await?
            {
                return Ok(true);
            }
        }

        Ok(false)
    }

    async fn backtrack_package(
        &mut self,
        state: &mut ResolutionState,
        package_name: &str,
        include_build_requires: bool,
    ) -> Result<bool, RezCoreError> {
        let Some(current) = state
            .resolved_packages
            .iter()
            .find(|resolved| resolved.package.name == package_name)
            .cloned()
        else {
            return Ok(false);
        };
        let Some(requirements) = state.active_requirements.get(package_name).cloned() else {
            return Ok(false);
        };
        let Some(primary_requirement) = requirements.first() else {
            return Ok(false);
        };

        let mut state_without_current = state.clone();
        state_without_current.reject_resolution(&current);
        state_without_current.remove_resolved_package(package_name);
        let candidates = self.find_candidate_packages(primary_requirement).await?;
        for candidate in candidates {
            if candidate.version.as_ref().is_some_and(|version| {
                requirements
                    .iter()
                    .any(|requirement| !requirement.is_satisfied_by(version))
            }) || state_without_current
                .check_explicit_conflicts(&candidate)
                .is_some()
            {
                continue;
            }

            let mut trial_state = state_without_current.clone();
            let requirement = current
                .satisfying_requirement
                .as_ref()
                .unwrap_or(primary_requirement);
            if let Ok((resolved_info, dependencies, conflicts)) = self
                .try_resolve_with_candidate(
                    &mut trial_state,
                    &candidate,
                    requirement,
                    include_build_requires,
                )
                .await
            {
                if trial_state.is_resolution_rejected(&resolved_info) {
                    continue;
                }
                trial_state.add_resolved_package(resolved_info);
                trial_state.set_package_requirements(
                    package_name.to_string(),
                    dependencies,
                    conflicts,
                );
                trial_state.backtrack_steps += 1;
                *state = trial_state;
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Find candidate packages that could satisfy a requirement
    async fn find_candidate_packages(
        &mut self,
        requirement: &Requirement,
    ) -> Result<Vec<Arc<Package>>, RezCoreError> {
        // Check cache first
        if let Some(cached) = self.package_cache.get(&requirement.name) {
            return Ok(self.filter_and_sort_candidates(cached, requirement));
        }

        // Search repositories
        let packages = self
            .repository_manager
            .find_packages(&requirement.name)
            .await?;

        // Cache the results
        self.package_cache
            .insert(requirement.name.clone(), packages.clone());

        Ok(self.filter_and_sort_candidates(&packages, requirement))
    }

    /// Filter candidate packages based on version constraints and sort them
    fn filter_and_sort_candidates(
        &self,
        packages: &[Arc<Package>],
        requirement: &Requirement,
    ) -> Vec<Arc<Package>> {
        let mut candidates: Vec<Arc<Package>> = packages
            .iter()
            .filter(|pkg| {
                if let Some(ref version) = pkg.version {
                    // Respect allow_prerelease flag
                    if !self.config.allow_prerelease && version.is_prerelease() {
                        return false;
                    }
                    requirement.is_satisfied_by(version)
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        // Sort by version: latest first (prefer latest behavior)
        if self.config.prefer_latest {
            candidates.sort_by(|a, b| match (&b.version, &a.version) {
                (Some(bv), Some(av)) => bv.cmp(av),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        } else {
            // oldest first
            candidates.sort_by(|a, b| match (&a.version, &b.version) {
                (Some(av), Some(bv)) => av.cmp(bv),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            });
        }

        candidates
    }

    /// Try to resolve using a specific candidate package
    async fn try_resolve_with_candidate(
        &mut self,
        state: &mut ResolutionState,
        candidate: &Arc<Package>,
        requirement: &Requirement,
        include_build_requires: bool,
    ) -> Result<(ResolvedPackageInfo, Vec<Requirement>, Vec<Requirement>), RezCoreError> {
        // Determine which variant to use (if package has variants)
        let variant_index = self.select_variant(candidate, state);
        state.variants_evaluated += candidate.variants.len().max(1);
        if !candidate.variants.is_empty() && variant_index.is_none() {
            return Err(RezCoreError::Solver(format!(
                "No compatible variant for package {}",
                candidate.name
            )));
        }

        // Get the effective requires list (base + variant-specific)
        let effective_requires =
            self.get_effective_requires(candidate, variant_index, include_build_requires);

        // Create resolved package info
        let resolved_info = ResolvedPackageInfo {
            package: candidate.clone(),
            variant_index,
            requested: state.is_original_requirement(requirement),
            required_by: vec![requirement.name.clone()],
            satisfying_requirement: Some(requirement.clone()),
        };

        let mut dependencies = Vec::new();
        let mut conflicts = Vec::new();
        for dep_req_str in &effective_requires {
            let (conflict, dep_requirement) = Self::parse_variant_requirement(dep_req_str)
                .map_err(|e| {
                    RezCoreError::RequirementParse(format!(
                        "Invalid requirement '{}': {}",
                        dep_req_str, e
                    ))
                })?;
            if conflict {
                conflicts.push(dep_requirement);
                continue;
            }
            dependencies.push(dep_requirement);
        }

        Ok((resolved_info, dependencies, conflicts))
    }

    /// Select the best variant for a package given current resolution state
    fn select_variant(&self, package: &Package, state: &ResolutionState) -> Option<usize> {
        if package.variants.is_empty() {
            return None;
        }

        package
            .variants
            .iter()
            .enumerate()
            .filter(|(_, requirements)| self.variant_is_compatible(requirements, state))
            .max_by_key(|(index, requirements)| {
                let positive: Vec<_> = requirements
                    .iter()
                    .filter_map(|raw| Self::parse_variant_requirement(raw).ok())
                    .filter(|(conflict, _)| !conflict)
                    .map(|(_, requirement)| requirement)
                    .collect();
                let resolved_matches = positive
                    .iter()
                    .filter(|requirement| {
                        state.resolved_packages.iter().any(|resolved| {
                            resolved.package.name == requirement.name
                                && resolved
                                    .package
                                    .version
                                    .as_ref()
                                    .is_some_and(|version| requirement.is_satisfied_by(version))
                        })
                    })
                    .count();
                let shared = positive
                    .iter()
                    .filter(|requirement| {
                        state
                            .original_requirements
                            .iter()
                            .any(|original| original.name == requirement.name)
                    })
                    .count();
                let resolved_version_priority: Vec<_> = positive
                    .iter()
                    .filter(|requirement| {
                        state
                            .resolved_packages
                            .iter()
                            .any(|resolved| resolved.package.name == requirement.name)
                    })
                    .map(|requirement| {
                        (
                            requirement
                                .version_constraint
                                .as_ref()
                                .and_then(Self::constraint_version_floor),
                            requirement.name.clone(),
                        )
                    })
                    .collect();
                (
                    resolved_matches,
                    shared,
                    std::cmp::Reverse(positive.len()),
                    resolved_version_priority,
                    std::cmp::Reverse(*index),
                )
            })
            .map(|(index, _)| index)
    }

    fn constraint_version_floor(constraint: &VersionConstraint) -> Option<Version> {
        match constraint {
            VersionConstraint::Exact(version)
            | VersionConstraint::GreaterThan(version)
            | VersionConstraint::GreaterThanOrEqual(version)
            | VersionConstraint::Compatible(version)
            | VersionConstraint::Range(version, _)
            | VersionConstraint::Prefix(version) => Some(version.clone()),
            VersionConstraint::Multiple(constraints)
            | VersionConstraint::Alternative(constraints) => constraints
                .iter()
                .filter_map(Self::constraint_version_floor)
                .max(),
            VersionConstraint::LessThan(_)
            | VersionConstraint::LessThanOrEqual(_)
            | VersionConstraint::Exclude(_)
            | VersionConstraint::Wildcard(_)
            | VersionConstraint::Any => None,
        }
    }

    fn variant_is_compatible(&self, requirements: &[String], state: &ResolutionState) -> bool {
        requirements.iter().all(|raw| {
            let Ok((conflict, requirement)) = Self::parse_variant_requirement(raw) else {
                return false;
            };

            let resolved = state
                .resolved_packages
                .iter()
                .find(|resolved| resolved.package.name == requirement.name);
            if let Some(version) = resolved.and_then(|resolved| resolved.package.version.as_ref()) {
                let satisfies = requirement.is_satisfied_by(version);
                if conflict || satisfies {
                    return conflict != satisfies;
                }
            }

            if !conflict
                && state
                    .active_requirements
                    .get(&requirement.name)
                    .is_some_and(|active| {
                        active.iter().any(|active_requirement| {
                            !Self::requirements_may_intersect(&requirement, active_requirement)
                        })
                    })
            {
                return false;
            }

            true
        })
    }

    fn requirements_may_intersect(left: &Requirement, right: &Requirement) -> bool {
        let left_floor = left
            .version_constraint
            .as_ref()
            .and_then(Self::constraint_version_floor);
        let right_floor = right
            .version_constraint
            .as_ref()
            .and_then(Self::constraint_version_floor);
        match (left_floor, right_floor) {
            (Some(left_floor), Some(right_floor)) => {
                left.is_satisfied_by(&right_floor) || right.is_satisfied_by(&left_floor)
            }
            _ => true,
        }
    }

    fn parse_variant_requirement(raw: &str) -> Result<(bool, Requirement), String> {
        let (conflict, requirement) = raw
            .strip_prefix('!')
            .map_or((false, raw), |requirement| (true, requirement));
        requirement
            .parse()
            .map(|requirement| (conflict, requirement))
    }

    /// Get the effective requires list, merging base requires with variant requires
    fn get_effective_requires(
        &self,
        package: &Package,
        variant_index: Option<usize>,
        include_build_requires: bool,
    ) -> Vec<String> {
        let mut requires = package.requires.clone();

        if include_build_requires {
            for build_requirement in &package.build_requires {
                if !requires.contains(build_requirement) {
                    requires.push(build_requirement.clone());
                }
            }
        }

        if let Some(idx) = variant_index
            && let Some(variant_reqs) = package.variants.get(idx)
        {
            // Variant requires are appended to base requires
            for vreq in variant_reqs {
                if !requires.contains(vreq) {
                    requires.push(vreq.clone());
                }
            }
        }

        requires
    }
}
