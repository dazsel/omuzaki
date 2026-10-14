//! Graph helpers for the structural checks, all iterative: a hostile file can be 100k nodes deep or chain 100k
//! constraints, and recursion would overflow the stack.

/// Ancestor queries in O(1) after one O(n) pass: an Euler-tour numbering of the forest the parent table describes.
///
/// Walking up the parents per query is quadratic: a valid hostile file can name spring chains over one deep spine so
/// that every pair costs a walk to the root. Nodes on a parent cycle are reachable from no root and so lie below
/// nothing but themselves; `scene.node_cycle` already rejects such scenes, so that only decides which findings
/// come with it.
pub(super) struct Tour {
    enter: Vec<u32>,
    exit: Vec<u32>,
}

impl Tour {
    /// Marks a node no root reaches.
    const UNREACHED: u32 = u32::MAX;

    /// Numbers the nodes with one depth-first search over an explicit stack. Children are the inverse of `parent`,
    /// so a node listed under several parents stays with the last (the table keeps one parent per node).
    pub(super) fn new(parent: &[Option<usize>]) -> Self {
        let n = parent.len();
        let (start, flat) = csr(n, parent.iter().enumerate().filter_map(|(child, p)| p.map(|p| (p, child))));
        // The next child of each node to visit.
        let mut next = start.clone();
        let mut enter = vec![Self::UNREACHED; n];
        let mut exit = vec![Self::UNREACHED; n];
        // At most n nodes are numbered, so u32 holds the clock for any scene that fits in memory; saturate rather than
        // wrap on a scene that doesn't (the answers are then wrong only for scenes of 4 billion nodes).
        let mut clock = 0u32;
        let mut stack = Vec::new();
        for root in (0..n).filter(|&i| parent[i].is_none()) {
            enter[root] = clock;
            clock = clock.saturating_add(1);
            stack.push(root);
            while let Some(&top) = stack.last() {
                if next[top] < start[top + 1] {
                    let child = flat[next[top]];
                    next[top] += 1;
                    enter[child] = clock;
                    clock = clock.saturating_add(1);
                    stack.push(child);
                } else {
                    exit[top] = clock;
                    stack.pop();
                }
            }
        }
        Self { enter, exit }
    }

    /// True if `node` is `ancestor` or lies below it. Out-of-range or unreachable nodes are below only themselves.
    pub(super) fn is_below(&self, node: usize, ancestor: usize) -> bool {
        if node == ancestor {
            return true;
        }
        match (self.enter.get(node), self.exit.get(node), self.enter.get(ancestor), self.exit.get(ancestor)) {
            (Some(&en), Some(&ex), Some(&ea), Some(&xa)) if en != Self::UNREACHED && ea != Self::UNREACHED => {
                ea <= en && ex <= xa
            }
            _ => false,
        }
    }
}

/// Strongly connected components of the directed graph on `n` nodes with the given `(from, to)` edges: the component
/// id of every node. Ids are dense and arbitrary; two nodes share one exactly when each reaches the other. Tarjan's
/// algorithm over explicit stacks. Every edge end must be below `n`.
pub(super) fn components(n: usize, edges: impl Iterator<Item = (usize, usize)> + Clone) -> Vec<usize> {
    const NONE: usize = usize::MAX;
    let (start, to) = csr(n, edges);
    let (mut index, mut low, mut comp) = (vec![NONE; n], vec![0usize; n], vec![NONE; n]);
    let (mut clock, mut comps) = (0usize, 0usize);
    let mut open = Vec::new(); // Tarjan's component stack
    let mut work: Vec<(usize, usize)> = Vec::new(); // (node, next edge to follow)
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        index[root] = clock;
        low[root] = clock;
        clock += 1;
        open.push(root);
        work.push((root, start[root]));
        while let Some(&(v, e)) = work.last() {
            if e < start[v + 1] {
                if let Some(top) = work.last_mut() {
                    top.1 += 1;
                }
                let w = to[e];
                if index[w] == NONE {
                    index[w] = clock;
                    low[w] = clock;
                    clock += 1;
                    open.push(w);
                    work.push((w, start[w]));
                } else if comp[w] == NONE {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    while let Some(w) = open.pop() {
                        comp[w] = comps;
                        if w == v {
                            break;
                        }
                    }
                    comps += 1;
                }
            }
        }
    }
    comp
}

/// Adjacency in compressed form: `start[v]..start[v + 1]` indexes the targets of `v`'s edges in the returned vector.
/// Three allocations however wide or deep the graph is.
fn csr(n: usize, edges: impl Iterator<Item = (usize, usize)> + Clone) -> (Vec<usize>, Vec<usize>) {
    let mut start = vec![0usize; n + 1];
    for (from, _) in edges.clone() {
        start[from + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut to = vec![0usize; start[n]];
    for (from, target) in edges {
        to[fill[from]] = target;
        fill[from] += 1;
    }
    (start, to)
}
