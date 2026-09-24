pub struct Graph {
    offsets: Vec<u32>,
    targets: Vec<u32>,
    dependents: Vec<u32>,
}

impl Graph {
    pub fn new(nodes: usize, edges: &[(u32, u32)]) -> Self {
        let mut offsets = vec![0u32; nodes + 1];
        let mut dependents = vec![0u32; nodes];
        for &(from, to) in edges {
            offsets[from as usize + 1] += 1;
            dependents[to as usize] += 1;
        }
        for node in 0..nodes {
            offsets[node + 1] += offsets[node];
        }

        let mut next: Vec<u32> = offsets[..nodes].to_vec();
        let mut targets = vec![0u32; edges.len()];
        for &(from, to) in edges {
            let slot = &mut next[from as usize];
            targets[*slot as usize] = to;
            *slot += 1;
        }

        Self {
            offsets,
            targets,
            dependents,
        }
    }

    pub fn len(&self) -> usize {
        self.dependents.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dependents.is_empty()
    }

    pub fn dependencies(&self, node: usize) -> &[u32] {
        &self.targets[self.offsets[node] as usize..self.offsets[node + 1] as usize]
    }

    pub fn dependents_first(&self) -> Option<Vec<u32>> {
        let mut waiting = self.dependents.clone();
        let mut ready: Vec<u32> = (0..self.len() as u32)
            .filter(|&node| waiting[node as usize] == 0)
            .collect();
        let mut order = Vec::with_capacity(self.len());

        while let Some(node) = ready.pop() {
            order.push(node);
            for &dependency in self.dependencies(node as usize) {
                let count = &mut waiting[dependency as usize];
                *count -= 1;
                if *count == 0 {
                    ready.push(dependency);
                }
            }
        }

        (order.len() == self.len()).then_some(order)
    }

    pub fn frontier(&self, order: &[u32], stops: impl Fn(usize) -> bool) -> Vec<usize> {
        let mut reached: Vec<bool> = self.dependents.iter().map(|&count| count == 0).collect();
        let mut met = vec![false; self.len()];

        for &node in order {
            let node = node as usize;
            if !reached[node] {
                continue;
            }
            if stops(node) {
                met[node] = true;
                continue;
            }
            for &dependency in self.dependencies(node) {
                reached[dependency as usize] = true;
            }
        }

        met.iter()
            .enumerate()
            .filter_map(|(node, &met)| met.then_some(node))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frontier(nodes: usize, edges: &[(u32, u32)], stops: &[usize]) -> Option<Vec<usize>> {
        let graph = Graph::new(nodes, edges);
        let order = graph.dependents_first()?;
        Some(graph.frontier(&order, |node| stops.contains(&node)))
    }

    #[test]
    fn the_dependencies_of_a_node_are_the_edges_it_starts() {
        let graph = Graph::new(3, &[(0, 1), (2, 1), (0, 2)]);

        assert_eq!(graph.dependencies(0), [1, 2]);
        assert_eq!(graph.dependencies(1), [] as [u32; 0]);
        assert_eq!(graph.dependencies(2), [1]);
    }

    #[test]
    fn every_dependent_comes_before_what_it_depends_on() {
        let edges = [(0, 1), (1, 2), (0, 2), (3, 1)];
        let order = Graph::new(4, &edges).dependents_first().unwrap();

        let position = |node: u32| order.iter().position(|&n| n == node).unwrap();
        for (from, to) in edges {
            assert!(
                position(from) < position(to),
                "{from} must come before {to}"
            );
        }
    }

    #[test]
    fn a_cycle_has_no_order() {
        assert!(
            Graph::new(3, &[(0, 1), (1, 2), (2, 1)])
                .dependents_first()
                .is_none()
        );
    }

    #[test]
    fn a_node_that_depends_on_itself_has_no_order() {
        assert!(Graph::new(1, &[(0, 0)]).dependents_first().is_none());
    }

    #[test]
    fn an_empty_graph_has_an_empty_order() {
        let graph = Graph::new(0, &[]);

        assert!(graph.is_empty());
        assert_eq!(graph.dependents_first(), Some(Vec::new()));
    }

    #[test]
    fn the_frontier_stops_at_the_first_stopping_node_on_a_chain() {
        assert_eq!(
            frontier(4, &[(0, 1), (1, 2), (2, 3)], &[1, 2, 3]),
            Some(vec![1])
        );
    }

    #[test]
    fn the_frontier_passes_through_nodes_that_do_not_stop_it() {
        assert_eq!(
            frontier(4, &[(0, 1), (1, 2), (2, 3)], &[2, 3]),
            Some(vec![2])
        );
    }

    #[test]
    fn a_node_reached_directly_is_met_even_when_another_met_node_needs_it() {
        let edges = [(0, 1), (0, 2), (2, 1)];

        assert_eq!(frontier(3, &edges, &[1, 2]), Some(vec![1, 2]));
    }

    #[test]
    fn a_node_reached_only_through_a_met_node_is_not_met() {
        let edges = [(0, 1), (1, 2), (2, 3)];

        assert_eq!(frontier(4, &edges, &[1, 3]), Some(vec![1]));
    }

    #[test]
    fn a_top_level_node_that_stops_is_met() {
        assert_eq!(frontier(2, &[(0, 1)], &[0, 1]), Some(vec![0]));
    }

    #[test]
    fn every_top_level_node_is_a_starting_point() {
        let edges = [(0, 2), (1, 3)];

        assert_eq!(frontier(4, &edges, &[2, 3]), Some(vec![2, 3]));
    }

    #[test]
    fn a_repeated_edge_is_counted_once_per_copy() {
        assert_eq!(frontier(2, &[(0, 1), (0, 1)], &[1]), Some(vec![1]));
    }
}
