//! Dependency resolution: a pure function from manifests to a load order.
//!
//! No file system, no clock, no randomness: the same set of manifests gives the same answer on
//! every machine and run, whatever order they are passed in (decision 0014, section a).

use std::collections::{BTreeSet, VecDeque};

use crate::error::ResolveError;
use crate::manifest::{ApiVersion, ModId, ModManifest};

/// Checks a set of mods and returns their ids in load order: every mod comes after the mods it
/// depends on, and among mods that are free to go in any order the smaller id goes first.
///
/// Checks, in this order, and collects every problem instead of stopping at the first:
/// 1. each mod alone: a duplicate id, an API the game does not provide (see
///    [`ApiVersion::is_supported_by`]);
/// 2. each dependency: the mod is missing, or its version is outside the requirement;
/// 3. circles of dependencies, each reported once with a shortest path. The whole function is
///    O(V + E) apart from the sorting of the input.
///
/// With duplicate ids the mod with the lowest version stands for the id in the later checks, so
/// the errors do not depend on the order of `mods`. A dependency on a missing mod is not an edge
/// of the graph, so it never also shows up as a circle.
///
/// ```
/// use vox_mods::{MOD_API_VERSION, ModManifest, resolve};
///
/// let base = ModManifest::parse(br#"{"format":1,"id":"base","version":"0.1.0","api":"1.0"}"#)
///     .unwrap();
/// let ruby = ModManifest::parse(
///     br#"{"format":1,"id":"ruby","version":"1.0.0","api":"1.0","dependencies":{"base":"^0.1"}}"#,
/// )
/// .unwrap();
/// let order = resolve(&[ruby, base], MOD_API_VERSION).unwrap();
/// let names: Vec<&str> = order.iter().map(|id| id.as_str()).collect();
/// assert_eq!(names, ["base", "ruby"]);
/// ```
///
/// # Errors
/// All [`ResolveError`]s found, never an empty list.
pub fn resolve(
    mods: &[ModManifest],
    game_api: ApiVersion,
) -> Result<Vec<ModId>, Vec<ResolveError>> {
    let mut errors = Vec::new();
    let nodes = unique_by_id(mods, &mut errors);

    for node in &nodes {
        check_api(node, game_api, &mut errors);
        check_dependencies(node, &nodes, &mut errors);
    }

    let graph = Graph::new(&nodes);
    let order = graph.topological_order();
    graph.report_cycles(&nodes, &mut errors);

    if errors.is_empty() {
        Ok(order.into_iter().map(|i| nodes[i].id().clone()).collect())
    } else {
        Err(errors)
    }
}

/// One manifest per id, sorted by id. A repeated id is reported and the lowest version stays.
fn unique_by_id<'a>(
    mods: &'a [ModManifest],
    errors: &mut Vec<ResolveError>,
) -> Vec<&'a ModManifest> {
    let mut sorted: Vec<&ModManifest> = mods.iter().collect();
    sorted.sort_by(|a, b| (a.id(), a.version()).cmp(&(b.id(), b.version())));
    let mut unique: Vec<&ModManifest> = Vec::with_capacity(sorted.len());
    let mut counts: Vec<usize> = Vec::with_capacity(sorted.len());
    for manifest in sorted {
        match unique.last() {
            Some(last) if last.id() == manifest.id() => {
                if let Some(count) = counts.last_mut() {
                    *count += 1;
                }
            }
            _ => {
                unique.push(manifest);
                counts.push(1);
            }
        }
    }
    for (manifest, count) in unique.iter().zip(counts) {
        if count > 1 {
            errors.push(ResolveError::DuplicateId {
                id: manifest.id().clone(),
                count,
            });
        }
    }
    unique
}

fn check_api(node: &ModManifest, game_api: ApiVersion, errors: &mut Vec<ResolveError>) {
    if !node.api().is_supported_by(game_api) {
        errors.push(ResolveError::UnsupportedApi {
            id: node.id().clone(),
            mod_api: node.api(),
            game_api,
        });
    }
}

/// Reports each dependency that is missing or has a version outside the requirement.
/// `nodes` is sorted by id.
fn check_dependencies(node: &ModManifest, nodes: &[&ModManifest], errors: &mut Vec<ResolveError>) {
    for (dependency, requirement) in node.dependencies() {
        match find(nodes, dependency) {
            None => errors.push(ResolveError::MissingDependency {
                id: node.id().clone(),
                dependency: dependency.clone(),
                requirement: requirement.to_string(),
            }),
            Some(index) => {
                let found = nodes[index].version();
                if !requirement.matches(found) {
                    errors.push(ResolveError::VersionMismatch {
                        id: node.id().clone(),
                        dependency: dependency.clone(),
                        requirement: requirement.to_string(),
                        found: found.to_string(),
                    });
                }
            }
        }
    }
}

/// The index of `id` in `nodes`, which is sorted by id.
fn find(nodes: &[&ModManifest], id: &ModId) -> Option<usize> {
    nodes.binary_search_by(|node| node.id().cmp(id)).ok()
}

/// Marks a mod the circle search has not reached yet.
const UNVISITED: usize = usize::MAX;

/// The dependency graph over indices into the sorted `nodes`; edges only to mods that exist.
/// Index order is id order, which is what makes every tie-break deterministic.
struct Graph {
    /// `dependencies[i]`: the mods `i` depends on, ascending.
    dependencies: Vec<Vec<usize>>,
    /// `dependents[i]`: the mods that depend on `i`, ascending.
    dependents: Vec<Vec<usize>>,
}

impl Graph {
    fn new(nodes: &[&ModManifest]) -> Self {
        let mut dependencies = vec![Vec::new(); nodes.len()];
        let mut dependents = vec![Vec::new(); nodes.len()];
        for (index, node) in nodes.iter().enumerate() {
            // The map is ordered by id, so the indices come out ascending.
            for dependency in node.dependencies().keys() {
                if let Some(target) = find(nodes, dependency) {
                    dependencies[index].push(target);
                    dependents[target].push(index);
                }
            }
        }
        Self {
            dependencies,
            dependents,
        }
    }

    /// Kahn's algorithm, always taking the smallest ready index. Mods in a circle, or depending
    /// on one, are never ready and are left out of the order (the caller reports the circles
    /// and fails, so a partial order is never used).
    fn topological_order(&self) -> Vec<usize> {
        let mut waiting: Vec<usize> = self.dependencies.iter().map(Vec::len).collect();
        let mut ready: BTreeSet<usize> = (0..waiting.len()).filter(|&i| waiting[i] == 0).collect();
        let mut order = Vec::with_capacity(waiting.len());
        while let Some(next) = ready.pop_first() {
            order.push(next);
            for &dependent in &self.dependents[next] {
                waiting[dependent] -= 1;
                if waiting[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
        }
        order
    }

    /// Pushes one error per circle: each strongly connected component with more than one mod,
    /// or one mod that depends on itself. Mods that merely depend on a circle are not part of
    /// one and are not reported. Circles come out ordered by their smallest id.
    fn report_cycles(&self, nodes: &[&ModManifest], errors: &mut Vec<ResolveError>) {
        let mut circles: Vec<Vec<usize>> = self
            .strongly_connected_components()
            .into_iter()
            .filter(|component| {
                component.len() > 1 || self.dependencies[component[0]].contains(&component[0])
            })
            .map(|mut component| {
                component.sort_unstable();
                component
            })
            .collect();
        circles.sort_unstable();
        for component in circles {
            let mut members = vec![false; nodes.len()];
            for &i in &component {
                members[i] = true;
            }
            let path = self.shortest_cycle(component[0], &members);
            errors.push(ResolveError::Cycle {
                path: path.into_iter().map(|i| nodes[i].id().clone()).collect(),
            });
        }
    }

    /// Tarjan's algorithm, written with an explicit stack so a long chain of mods cannot
    /// overflow the call stack. O(V + E). Each component is returned once, in no useful order
    /// (the caller sorts); the result does not depend on the order of the input because the
    /// indices are in id order and edges are ascending.
    fn strongly_connected_components(&self) -> Vec<Vec<usize>> {
        let count = self.dependencies.len();
        let mut index = vec![UNVISITED; count];
        let mut lowlink = vec![0; count];
        let mut on_stack = vec![false; count];
        let mut stack: Vec<usize> = Vec::new();
        let mut components = Vec::new();
        let mut next_index = 0;
        for root in 0..count {
            if index[root] != UNVISITED {
                continue;
            }
            // Each frame is a mod and the position of the next edge to follow.
            let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
            index[root] = next_index;
            lowlink[root] = next_index;
            next_index += 1;
            stack.push(root);
            on_stack[root] = true;
            while let Some(&(node, edge)) = frames.last() {
                if let Some(&next) = self.dependencies[node].get(edge) {
                    if let Some(frame) = frames.last_mut() {
                        frame.1 += 1;
                    }
                    if index[next] == UNVISITED {
                        index[next] = next_index;
                        lowlink[next] = next_index;
                        next_index += 1;
                        stack.push(next);
                        on_stack[next] = true;
                        frames.push((next, 0));
                    } else if on_stack[next] {
                        lowlink[node] = lowlink[node].min(index[next]);
                    }
                    continue;
                }
                frames.pop();
                if lowlink[node] == index[node] {
                    let mut component = Vec::new();
                    while let Some(member) = stack.pop() {
                        on_stack[member] = false;
                        component.push(member);
                        if member == node {
                            break;
                        }
                    }
                    components.push(component);
                }
                if let Some(&(parent, _)) = frames.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[node]);
                }
            }
        }
        components
    }

    /// A shortest circle through `start` inside `members`, as `[start, .., last]` where `last`
    /// depends on `start`. Breadth first over ascending edges, so the choice is deterministic.
    fn shortest_cycle(&self, start: usize, members: &[bool]) -> Vec<usize> {
        let mut parent: Vec<Option<usize>> = vec![None; members.len()];
        let mut queue = VecDeque::from([start]);
        while let Some(current) = queue.pop_front() {
            for &next in &self.dependencies[current] {
                if !members[next] {
                    continue;
                }
                if next == start {
                    return path_back(start, current, &parent);
                }
                if parent[next].is_none() {
                    parent[next] = Some(current);
                    queue.push_back(next);
                }
            }
        }
        // Unreachable for a member of a circle; the single mod is the least misleading answer.
        vec![start]
    }
}

/// The path `[start, .., last]` found by following `parent` back from `last`.
fn path_back(start: usize, last: usize, parent: &[Option<usize>]) -> Vec<usize> {
    let mut path = vec![last];
    let mut current = last;
    while current != start {
        match parent[current] {
            Some(previous) => {
                path.push(previous);
                current = previous;
            }
            None => break,
        }
    }
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MOD_API_VERSION;

    /// A manifest built from JSON, so the tests go through the same validation as real files.
    fn m(id: &str, version: &str, deps: &[(&str, &str)]) -> ModManifest {
        with_api(id, version, "1.0", deps)
    }

    fn with_api(id: &str, version: &str, api: &str, deps: &[(&str, &str)]) -> ModManifest {
        let deps: Vec<String> = deps
            .iter()
            .map(|(name, req)| format!(r#""{name}": "{req}""#))
            .collect();
        let json = format!(
            r#"{{ "format": 1, "id": "{id}", "version": "{version}", "api": "{api}",
                 "dependencies": {{ {} }} }}"#,
            deps.join(", ")
        );
        ModManifest::parse(json.as_bytes()).unwrap()
    }

    fn ids(order: &[ModId]) -> Vec<&str> {
        order.iter().map(ModId::as_str).collect()
    }

    fn order(mods: &[ModManifest]) -> Vec<String> {
        let result = resolve(mods, MOD_API_VERSION).unwrap();
        ids(&result).into_iter().map(str::to_owned).collect()
    }

    fn errors(mods: &[ModManifest]) -> Vec<ResolveError> {
        resolve(mods, MOD_API_VERSION).unwrap_err()
    }

    fn id(text: &str) -> ModId {
        ModId::parse(text).unwrap()
    }

    fn path(names: &[&str]) -> ResolveError {
        ResolveError::Cycle {
            path: names.iter().map(|n| id(n)).collect(),
        }
    }

    #[test]
    fn no_mods_is_an_empty_order() {
        assert!(order(&[]).is_empty());
    }

    #[test]
    fn a_chain_loads_dependencies_first() {
        let mods = [
            m("gems", "1.2.0", &[("ruby", "^1")]),
            m("ruby", "1.0.0", &[("base", "^0.1")]),
            m("base", "0.1.0", &[]),
        ];
        assert_eq!(order(&mods), ["base", "ruby", "gems"]);
    }

    #[test]
    fn free_mods_are_ordered_by_id() {
        let mods = [
            m("zed", "1.0.0", &[]),
            m("alpha", "1.0.0", &[]),
            m("mid", "1.0.0", &[]),
        ];
        assert_eq!(order(&mods), ["alpha", "mid", "zed"]);
    }

    #[test]
    fn a_dependency_beats_the_id_order() {
        // `a` is the smallest id but needs `z`.
        let mods = [
            m("a", "1.0.0", &[("z", "*")]),
            m("b", "1.0.0", &[]),
            m("z", "1.0.0", &[]),
        ];
        assert_eq!(order(&mods), ["b", "z", "a"]);
    }

    #[test]
    fn diamond_dependencies_load_the_shared_root_once_and_first() {
        let mods = [
            m("top", "1.0.0", &[("left", "*"), ("right", "*")]),
            m("right", "1.0.0", &[("root", "*")]),
            m("left", "1.0.0", &[("root", "*")]),
            m("root", "1.0.0", &[]),
        ];
        assert_eq!(order(&mods), ["root", "left", "right", "top"]);
    }

    #[test]
    fn order_is_identical_over_shuffled_input() {
        let mods = vec![
            m("top", "1.0.0", &[("left", "*"), ("right", "*")]),
            m("right", "1.0.0", &[("root", "*")]),
            m("left", "1.0.0", &[("root", "*")]),
            m("root", "1.0.0", &[]),
            m("free_b", "1.0.0", &[]),
            m("free_a", "1.0.0", &[]),
            m("tail", "1.0.0", &[("top", "*")]),
        ];
        let expected = order(&mods);
        // A small deterministic generator, so the test needs no extra crate and never flakes.
        let mut state = 0x2545_F491_4F6C_DD1D_u64;
        let mut shuffled = mods.clone();
        for _ in 0..50 {
            for i in (1..shuffled.len()).rev() {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let j = usize::try_from(state >> 33).unwrap() % (i + 1);
                shuffled.swap(i, j);
            }
            assert_eq!(order(&shuffled), expected);
        }
        let mut reversed = mods;
        reversed.reverse();
        assert_eq!(order(&reversed), expected);
    }

    #[test]
    fn errors_are_identical_over_shuffled_input() {
        let mods = vec![
            m("a", "1.0.0", &[("b", "*")]),
            m("b", "1.0.0", &[("a", "*"), ("gone", "^1")]),
            m("c", "1.0.0", &[("a", "^2")]),
        ];
        let expected = errors(&mods);
        let mut reversed = mods;
        reversed.reverse();
        assert_eq!(errors(&reversed), expected);
    }

    #[test]
    fn missing_dependency_is_reported() {
        let mods = [m("ruby", "1.0.0", &[("base", "^0.1")])];
        assert_eq!(
            errors(&mods),
            [ResolveError::MissingDependency {
                id: id("ruby"),
                dependency: id("base"),
                requirement: "^0.1".to_owned()
            }]
        );
    }

    #[test]
    fn version_outside_the_requirement_is_reported() {
        let mods = [
            m("base", "0.2.0", &[]),
            m("ruby", "1.0.0", &[("base", "^0.1")]),
        ];
        assert_eq!(
            errors(&mods),
            [ResolveError::VersionMismatch {
                id: id("ruby"),
                dependency: id("base"),
                requirement: "^0.1".to_owned(),
                found: "0.2.0".to_owned()
            }]
        );
    }

    #[test]
    fn caret_tilde_and_exact_requirements() {
        // (requirement, installed version, accepted): a few rows per kind, the semver crate
        // tests its own matching.
        let cases = [
            ("^1.2.3", "1.9.0", true),
            ("^1.2.3", "2.0.0", false),
            ("^0.1", "0.2.0", false),
            ("~1.2.3", "1.2.9", true),
            ("~1.2.3", "1.3.0", false),
            ("=1.2.3", "1.2.4", false),
            // A bare version means caret, as in cargo.
            ("1.2.3", "1.4.0", true),
        ];
        for (requirement, version, accepted) in cases {
            let mods = [
                m("dep", version, &[]),
                m("user", "1.0.0", &[("dep", requirement)]),
            ];
            let result = resolve(&mods, MOD_API_VERSION);
            assert_eq!(result.is_ok(), accepted, "{requirement} vs {version}");
            if !accepted {
                assert!(matches!(
                    result.unwrap_err()[..],
                    [ResolveError::VersionMismatch { .. }]
                ));
            }
        }
    }

    #[test]
    fn prerelease_matches_only_a_requirement_that_names_one() {
        // Cargo rules: `^1.0` does not match `1.1.0-beta`, `^1.1.0-alpha` does.
        let plain = [
            m("dep", "1.1.0-beta", &[]),
            m("user", "1.0.0", &[("dep", "^1.0")]),
        ];
        assert!(matches!(
            errors(&plain)[..],
            [ResolveError::VersionMismatch { .. }]
        ));
        let naming = [
            m("dep", "1.1.0-beta", &[]),
            m("user", "1.0.0", &[("dep", "^1.1.0-alpha")]),
        ];
        assert_eq!(order(&naming), ["dep", "user"]);
    }

    #[test]
    fn api_acceptance_rule_applies_to_every_mod() {
        let game = ApiVersion::new(1, 2);
        for (api, accepted) in [
            ("1.0", true),
            ("1.2", true),
            ("1.3", false),
            ("2.0", false),
            ("0.9", false),
        ] {
            let mods = [with_api("a", "1.0.0", api, &[])];
            assert_eq!(resolve(&mods, game).is_ok(), accepted, "api {api}");
        }
        let mods = [with_api("a", "1.0.0", "1.3", &[])];
        assert_eq!(
            resolve(&mods, game).unwrap_err(),
            [ResolveError::UnsupportedApi {
                id: id("a"),
                mod_api: ApiVersion::new(1, 3),
                game_api: game
            }]
        );
    }

    #[test]
    fn duplicate_id_is_reported_with_its_count() {
        let mods = [
            m("a", "1.0.0", &[]),
            m("b", "1.0.0", &[]),
            m("a", "2.0.0", &[]),
        ];
        assert_eq!(
            errors(&mods),
            [ResolveError::DuplicateId {
                id: id("a"),
                count: 2
            }]
        );
        let three = [
            m("a", "1.0.0", &[]),
            m("a", "1.0.1", &[]),
            m("a", "1.0.2", &[]),
        ];
        assert_eq!(
            errors(&three),
            [ResolveError::DuplicateId {
                id: id("a"),
                count: 3
            }]
        );
    }

    #[test]
    fn duplicates_are_checked_against_the_lowest_version() {
        // Whatever the input order, `b` is judged against `a` 1.0.0, so it is the same error.
        let one = [
            m("a", "2.0.0", &[]),
            m("a", "1.0.0", &[]),
            m("b", "1.0.0", &[("a", "^2")]),
        ];
        let other = [
            m("b", "1.0.0", &[("a", "^2")]),
            m("a", "1.0.0", &[]),
            m("a", "2.0.0", &[]),
        ];
        assert_eq!(errors(&one), errors(&other));
        assert!(
            errors(&one)
                .iter()
                .any(|e| matches!(e, ResolveError::VersionMismatch { .. }))
        );
    }

    #[test]
    fn self_dependency_is_a_circle_of_one() {
        let mods = [m("a", "1.0.0", &[("a", "*")])];
        assert_eq!(errors(&mods), [path(&["a"])]);
        assert_eq!(errors(&mods)[0].to_string().matches("a -> a").count(), 1);
    }

    #[test]
    fn two_mods_in_a_circle() {
        let mods = [
            m("a", "1.0.0", &[("b", "*")]),
            m("b", "1.0.0", &[("a", "*")]),
        ];
        assert_eq!(errors(&mods), [path(&["a", "b"])]);
    }

    #[test]
    fn a_circle_of_three_names_every_mod_in_dependency_order() {
        let mods = [
            m("c", "1.0.0", &[("a", "*")]),
            m("a", "1.0.0", &[("b", "*")]),
            m("b", "1.0.0", &[("c", "*")]),
        ];
        assert_eq!(errors(&mods), [path(&["a", "b", "c"])]);
        assert_eq!(
            errors(&mods)[0].to_string(),
            "mods depend on each other in a circle: a -> b -> c -> a"
        );
    }

    #[test]
    fn two_circles_are_reported_together_and_dependents_are_not_circles() {
        let mods = [
            m("a", "1.0.0", &[("b", "*")]),
            m("b", "1.0.0", &[("a", "*")]),
            m("x", "1.0.0", &[("y", "*")]),
            m("y", "1.0.0", &[("x", "*")]),
            m("user", "1.0.0", &[("a", "*")]), // needs a circle, is not in one
            m("ok", "1.0.0", &[]),
        ];
        assert_eq!(errors(&mods), [path(&["a", "b"]), path(&["x", "y"])]);
    }

    #[test]
    fn the_shortest_circle_through_the_smallest_id_is_named() {
        // a -> b -> c -> a and a -> c -> a; the shorter one wins.
        let mods = [
            m("a", "1.0.0", &[("b", "*"), ("c", "*")]),
            m("b", "1.0.0", &[("c", "*")]),
            m("c", "1.0.0", &[("a", "*")]),
        ];
        assert_eq!(errors(&mods), [path(&["a", "c"])]);
    }

    #[test]
    fn all_problems_are_collected_at_once() {
        let mods = [
            with_api("old", "1.0.0", "2.0", &[]),
            m("dup", "1.0.0", &[]),
            m("dup", "1.0.1", &[]),
            m("needy", "1.0.0", &[("nowhere", "^1"), ("base", "^9")]),
            m("base", "0.1.0", &[]),
            m("p", "1.0.0", &[("q", "*")]),
            m("q", "1.0.0", &[("p", "*")]),
        ];
        let result = errors(&mods);
        assert_eq!(result.len(), 5, "{result:#?}");
        assert!(matches!(result[0], ResolveError::DuplicateId { .. }));
        assert!(
            result
                .iter()
                .any(|e| matches!(e, ResolveError::UnsupportedApi { .. }))
        );
        assert!(
            result
                .iter()
                .any(|e| matches!(e, ResolveError::MissingDependency { .. }))
        );
        assert!(
            result
                .iter()
                .any(|e| matches!(e, ResolveError::VersionMismatch { .. }))
        );
        assert_eq!(result[4], path(&["p", "q"]));
    }

    #[test]
    fn a_long_ring_and_a_long_chain_do_not_overflow_the_stack() {
        const LENGTH: usize = 5_000;
        let name = |i: usize| format!("m{i:05}");
        // A ring m0 -> m1 -> ... -> m0, one circle of LENGTH mods.
        let ring: Vec<ModManifest> = (0..LENGTH)
            .map(|i| m(&name(i), "1.0.0", &[(&name((i + 1) % LENGTH), "*")]))
            .collect();
        let result = errors(&ring);
        assert!(matches!(&result[..], [ResolveError::Cycle { path }] if path.len() == LENGTH));
        // A chain m0 -> m1 -> ... loads in reverse.
        let chain: Vec<ModManifest> = (0..LENGTH)
            .map(|i| {
                let deps = if i + 1 < LENGTH {
                    vec![(name(i + 1), "*")]
                } else {
                    vec![]
                };
                let deps: Vec<(&str, &str)> = deps.iter().map(|(n, r)| (n.as_str(), *r)).collect();
                m(&name(i), "1.0.0", &deps)
            })
            .collect();
        assert_eq!(order(&chain)[0], name(LENGTH - 1));
    }

    #[test]
    fn a_missing_dependency_is_not_also_a_circle() {
        let mods = [m("a", "1.0.0", &[("gone", "*")])];
        assert_eq!(errors(&mods).len(), 1);
    }

    #[test]
    fn messages_are_readable() {
        let mods = [m("ruby", "1.0.0", &[("base", "^0.1")])];
        assert_eq!(
            errors(&mods)[0].to_string(),
            "mod `ruby` needs mod `base` ^0.1, which is not installed"
        );
        let mods = [with_api("a", "1.0.0", "1.3", &[])];
        let message = resolve(&mods, MOD_API_VERSION).unwrap_err()[0].to_string();
        assert!(
            message.contains("API 1.3") && message.contains("API 1.0"),
            "{message}"
        );
    }
}
