//! Temporal knowledge graph implementation using petgraph.
//!
//! The graph uses `StableGraph` for stable node/edge indices across mutations.
//! Each edge carries temporal metadata (valid_from, valid_until) for
//! time-aware relationship tracking.

use petgraph::stable_graph::{NodeIndex, StableGraph};
use std::collections::HashMap;

use super::entities::{GraphEntity, GraphRelation, GraphSnapshot};

/// Edge data in the temporal graph.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TemporalEdge {
    pub relation_type: String,
    pub valid_from: f64,
    pub valid_until: Option<f64>,
    pub confidence: f32,
    pub source_segment_id: String,
}

/// A temporal knowledge graph backed by petgraph's StableGraph.
#[allow(dead_code)]
pub struct TemporalKnowledgeGraph {
    /// The underlying petgraph graph.
    graph: StableGraph<GraphEntity, TemporalEdge>,
    /// Index from entity name (lowercased) to node index.
    name_index: HashMap<String, NodeIndex>,
    /// Event counter for generating unique IDs.
    event_counter: u64,
}

impl TemporalKnowledgeGraph {
    /// Create a new empty temporal knowledge graph.
    pub fn new() -> Self {
        Self {
            graph: StableGraph::new(),
            name_index: HashMap::new(),
            event_counter: 0,
        }
    }

    /// Get the current number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Get the current number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Take a snapshot of the current graph state for frontend rendering.
    pub fn snapshot(&self) -> GraphSnapshot {
        let entities: Vec<GraphEntity> = self
            .graph
            .node_indices()
            .filter_map(|idx| self.graph.node_weight(idx).cloned())
            .collect();

        let relations: Vec<GraphRelation> = self
            .graph
            .edge_indices()
            .filter_map(|idx| {
                let (src, tgt) = self.graph.edge_endpoints(idx)?;
                let edge = self.graph.edge_weight(idx)?;
                let src_entity = self.graph.node_weight(src)?;
                let tgt_entity = self.graph.node_weight(tgt)?;
                Some(GraphRelation {
                    id: format!("e{}", idx.index()),
                    source_id: src_entity.id.clone(),
                    target_id: tgt_entity.id.clone(),
                    relation_type: edge.relation_type.clone(),
                    valid_from: edge.valid_from,
                    valid_until: edge.valid_until,
                    confidence: edge.confidence,
                    source_segment_id: edge.source_segment_id.clone(),
                })
            })
            .collect();

        GraphSnapshot {
            node_count: entities.len(),
            edge_count: relations.len(),
            last_updated: 0.0, // TODO: Track actual update time
            entities,
            relations,
        }
    }

    // TODO: Add methods:
    // - add_entity(name, entity_type, timestamp) -> NodeIndex
    // - add_relation(source, target, relation_type, timestamp, confidence, segment_id) -> EdgeIndex
    // - resolve_entity(name) -> Option<NodeIndex>  (fuzzy matching via strsim)
    // - invalidate_edge(edge_id, timestamp)
}

impl Default for TemporalKnowledgeGraph {
    fn default() -> Self {
        Self::new()
    }
}
