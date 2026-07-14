use crate::types::agent::{Chain, ReasoningPath};
use std::collections::{HashMap, HashSet, VecDeque};

/// BFS relation traversal over NSKG. Produces reasoning paths by walking
/// the relation graph up to `max_hops` hops from a set of source grids.
pub struct RelationEngine;

impl RelationEngine {
    /// Build reasoning paths from grid nodes via BFS over their relations.
    ///
    /// `nodes` is a slice of (node_id, relations, tags) tuples.
    /// Relations are `(target_id, target_type, relation_type, weight, interpretation)`.
    pub fn build_reasoning_paths(
        source_grid: &str,
        task_type_tags: &[String],
        all_nodes: &[NodeRelations],
        max_hops: u32,
    ) -> ReasoningPath {
        // Build O(1) index to avoid O(n²) lookup per BFS iteration.
        let node_index: HashMap<&str, &NodeRelations> = all_nodes
            .iter()
            .map(|n| (n.id.as_str(), n))
            .collect();

        let mut chains: Vec<Chain> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();

        visited.insert(source_grid.to_string());
        queue.push_back((source_grid.to_string(), 0));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_hops {
                continue;
            }

            // O(1) lookup via HashMap index.
            if let Some(node) = node_index.get(current.as_str()) {
                for rel in &node.relations {
                    if !visited.contains(&rel.target_id) {
                        visited.insert(rel.target_id.clone());
                        queue.push_back((rel.target_id.clone(), depth + 1));

                        chains.push(Chain {
                            source: current.clone(),
                            target: rel.target_id.clone(),
                            target_type: rel.target_type.clone(),
                            relation_type: rel.relation_type.clone(),
                            weight: rel.weight,
                            interpretation: rel.interpretation.clone(),
                        });
                    }
                }
            }
        }

        ReasoningPath {
            source_grid: source_grid.to_string(),
            chains,
            depth: max_hops,
            task_type_tags: task_type_tags.to_vec(),
        }
    }
}

/// A node with its relations, as read from Qdrant payload.
#[derive(Debug, Clone)]
pub struct NodeRelations {
    pub id: String,
    pub relations: Vec<RelationEdge>,
}

#[derive(Debug, Clone)]
pub struct RelationEdge {
    pub target_id: String,
    pub target_type: String,
    pub relation_type: String,
    pub weight: f64,
    pub interpretation: String,
}
