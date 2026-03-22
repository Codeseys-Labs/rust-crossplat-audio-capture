//! Knowledge graph module.
//!
//! Maintains a temporal knowledge graph using petgraph. Entities and relations
//! are extracted from transcript segments and stored with temporal metadata.

pub mod entities;
pub mod extraction;
pub mod temporal;

pub use entities::{
    ExtractedEntity, ExtractedRelation, ExtractionResult, GraphEntity, GraphLink, GraphNode,
    GraphRelation, GraphSnapshot, GraphStats,
};
pub use temporal::TemporalKnowledgeGraph;
