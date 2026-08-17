pub mod approval;
pub mod bash;
pub mod catalog;
pub mod file;
pub mod hashline;
pub mod metadata;
pub mod plan;
pub(crate) mod process;
pub mod python;
pub mod read_memo;
pub mod replace;
pub mod runner;
pub mod runtime_guidance;
#[cfg(feature = "python-sandbox")]
pub mod sandbox_python;
pub mod search;
pub mod semantic_capabilities;
pub mod snapshot;
pub mod surface;
pub mod todo;
pub mod vfs;

/// Shared DFS cycle detector for tool / capability dependency graphs.
///
/// Returns the first node that closes a cycle, or `None` when the graph is
/// acyclic. `dependencies` receives a node and returns its outgoing edges.
pub(crate) fn first_dependency_cycle<N, I, F, D>(nodes: I, dependencies: F) -> Option<N>
where
    N: Clone + Ord,
    I: IntoIterator<Item = N>,
    F: Fn(&N) -> D,
    D: IntoIterator<Item = N>,
{
    fn visit<N, F, D>(
        node: &N,
        dependencies: &F,
        visiting: &mut std::collections::BTreeSet<N>,
        visited: &mut std::collections::BTreeSet<N>,
    ) -> Option<N>
    where
        N: Clone + Ord,
        F: Fn(&N) -> D,
        D: IntoIterator<Item = N>,
    {
        if visited.contains(node) {
            return None;
        }
        if !visiting.insert(node.clone()) {
            return Some(node.clone());
        }
        for dependency in dependencies(node) {
            if let Some(cycle) = visit(&dependency, dependencies, visiting, visited) {
                return Some(cycle);
            }
        }
        visiting.remove(node);
        visited.insert(node.clone());
        None
    }

    let mut visiting = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    for node in nodes {
        if let Some(cycle) = visit(&node, &dependencies, &mut visiting, &mut visited) {
            return Some(cycle);
        }
    }
    None
}

#[cfg(test)]
mod edit_alignment_tests;
