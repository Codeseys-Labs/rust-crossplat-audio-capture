//! Entity and relation type definitions for the knowledge graph.
//!
//! These types are serialized to JSON and sent to the frontend.

use serde::{Deserialize, Serialize};

/// A node in the knowledge graph representing a named entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEntity {
    /// Stable node ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Entity type: PERSON, ORG, LOCATION, EVENT, CONCEPT, etc.
    pub entity_type: String,
    /// Number of times this entity has been mentioned.
    pub mention_count: u32,
    /// Timestamp of first mention (seconds since capture start).
    pub first_seen: f64,
    /// Timestamp of most recent mention.
    pub last_seen: f64,
    /// Alternative names / spellings.
    pub aliases: Vec<String>,
}

/// An edge in the knowledge graph representing a relationship between entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRelation {
    /// Stable edge ID.
    pub id: String,
    /// Source entity ID.
    pub source_id: String,
    /// Target entity ID.
    pub target_id: String,
    /// Relationship type: WORKS_AT, LOCATED_IN, KNOWS, etc.
    pub relation_type: String,
    /// When this relationship became valid.
    pub valid_from: f64,
    /// When this relationship ceased to be valid (None = still valid).
    pub valid_until: Option<f64>,
    /// Extraction confidence score.
    pub confidence: f32,
    /// ID of the transcript segment that sourced this relation.
    pub source_segment_id: String,
}

/// A point-in-time snapshot of the knowledge graph for frontend rendering.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphSnapshot {
    /// All entities (nodes) in the graph.
    pub entities: Vec<GraphEntity>,
    /// All relations (edges) in the graph.
    pub relations: Vec<GraphRelation>,
    /// Timestamp of the last graph update.
    pub last_updated: f64,
    /// Total number of nodes in the graph.
    pub node_count: usize,
    /// Total number of edges in the graph.
    pub edge_count: usize,
}

/// Result of entity extraction from a transcript segment (from LLM sidecar).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractionResult {
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

/// A raw entity extracted from text (before resolution).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    pub entity_type: String,
}

/// A raw relation extracted from text (before graph insertion).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedRelation {
    pub source: String,
    pub target: String,
    pub relation_type: String,
}
