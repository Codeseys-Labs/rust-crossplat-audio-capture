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
    /// Entity type: Person, Organization, Location, Event, Topic, Product, etc.
    pub entity_type: String,
    /// Number of times this entity has been mentioned.
    pub mention_count: u32,
    /// Timestamp of first mention (seconds since capture start).
    pub first_seen: f64,
    /// Timestamp of most recent mention.
    pub last_seen: f64,
    /// Alternative names / spellings.
    pub aliases: Vec<String>,
    /// Optional description for the entity.
    pub description: Option<String>,
    /// Which speakers mentioned this entity.
    pub speakers: Vec<String>,
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

// ---------------------------------------------------------------------------
// Frontend-friendly snapshot types (react-force-graph compatible)
// ---------------------------------------------------------------------------

/// A graph node ready for react-force-graph rendering.
#[derive(Debug, Clone, Serialize)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub entity_type: String,
    /// Node size (based on mention_count).
    pub val: f32,
    /// Color by entity_type.
    pub color: String,
    pub first_seen: f64,
    pub last_seen: f64,
    pub mention_count: u32,
    pub description: Option<String>,
}

/// A graph link ready for react-force-graph rendering.
#[derive(Debug, Clone, Serialize)]
pub struct GraphLink {
    /// Source node id.
    pub source: String,
    /// Target node id.
    pub target: String,
    pub relation_type: String,
    pub weight: f32,
    pub color: String,
    pub label: Option<String>,
}

/// Aggregate graph statistics.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GraphStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub total_episodes: u64,
}

/// A point-in-time snapshot of the knowledge graph for frontend rendering.
#[derive(Debug, Clone, Serialize, Default)]
pub struct GraphSnapshot {
    /// All nodes in react-force-graph format.
    pub nodes: Vec<GraphNode>,
    /// All links in react-force-graph format.
    pub links: Vec<GraphLink>,
    /// Aggregate statistics.
    pub stats: GraphStats,
}

/// Result of entity extraction from a transcript segment (from native LLM or rule-based).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

/// A raw entity extracted from text (before resolution).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    /// Entity type: "Person", "Organization", "Location", "Event", "Topic", "Product".
    pub entity_type: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// A raw relation extracted from text (before graph insertion).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedRelation {
    pub source: String,
    pub target: String,
    pub relation_type: String,
    #[serde(default)]
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

/// Map an entity type to a hex color string.
pub fn entity_type_color(entity_type: &str) -> &'static str {
    match entity_type.to_lowercase().as_str() {
        "person" => "#4CAF50",
        "organization" => "#2196F3",
        "location" => "#FF9800",
        "event" => "#9C27B0",
        "topic" => "#00BCD4",
        "product" => "#F44336",
        _ => "#607D8B",
    }
}

/// Map a relation type to a hex color string.
pub fn relation_type_color(relation_type: &str) -> &'static str {
    match relation_type.to_lowercase().as_str() {
        "works_at" | "employed_by" => "#4CAF50",
        "discussed" | "mentioned" => "#2196F3",
        "located_in" | "based_in" => "#FF9800",
        "related_to" => "#9E9E9E",
        _ => "#757575",
    }
}
