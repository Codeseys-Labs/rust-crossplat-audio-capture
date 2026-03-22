//! Rule-based entity extraction fallback.
//!
//! Provides rule-based entity extraction as a fallback when no LLM is available.
//! Uses regex patterns and heuristics to identify entities and relations.

use crate::graph::entities::{ExtractedEntity, ExtractedRelation, ExtractionResult};
use regex::Regex;

/// Rule-based entity extractor using regex patterns and heuristics.
///
/// This is the fallback extractor used when no native LLM model is available.
/// It identifies entities (persons, organizations, locations, topics) and
/// relations from transcript text using pre-compiled regex patterns.
pub struct RuleBasedExtractor {
    /// Matches capitalized word sequences (potential names/orgs).
    capitalized_phrase: Regex,
    /// Matches company suffixes like "Inc", "Corp", "Ltd", "LLC", etc.
    company_suffix: Regex,
    /// Matches common location indicators ("in <Place>", "at <Place>", etc.).
    location_patterns: Regex,
    /// Matches quoted terms (e.g., "machine learning", 'topic').
    quote_pattern: Regex,
}

impl RuleBasedExtractor {
    /// Create a new `RuleBasedExtractor` with pre-compiled regex patterns.
    pub fn new() -> Self {
        Self {
            // Match sequences of capitalized words (2+ words).
            // Excludes common sentence starters via post-filter.
            capitalized_phrase: Regex::new(r"\b([A-Z][a-z]+(?:\s+[A-Z][a-z]+)+)\b").unwrap(),

            // Company suffixes
            company_suffix: Regex::new(
                r"\b(\w+(?:\s+\w+)*\s+(?:Inc|Corp|Corporation|Ltd|LLC|Co|Company|Group|Technologies|Tech|Labs|Solutions|Systems|Services))\b"
            ).unwrap(),

            // Location indicators: "in <Place>", "at <Place>", "from <Place>"
            location_patterns: Regex::new(
                r"(?:in|at|from|near|based in)\s+([A-Z][a-z]+(?:\s+[A-Z][a-z]+)*)"
            ).unwrap(),

            // Quoted terms (topics)
            quote_pattern: Regex::new(r#"["']([^"']+)["']"#).unwrap(),
        }
    }

    /// Extract entities and relations from a transcript segment.
    ///
    /// `speaker` is the speaker label (e.g., "Speaker 1") — always added as a Person entity.
    /// `text` is the transcript text to extract from.
    pub fn extract(&self, speaker: &str, text: &str) -> ExtractionResult {
        let mut entities: Vec<ExtractedEntity> = Vec::new();
        let mut relations: Vec<ExtractedRelation> = Vec::new();
        let mut seen_entities: std::collections::HashSet<String> = std::collections::HashSet::new();

        // 1. Speaker is always a Person entity
        let speaker_name = speaker.to_string();
        entities.push(ExtractedEntity {
            name: speaker_name.clone(),
            entity_type: "Person".to_string(),
            description: None,
        });
        seen_entities.insert(speaker_name.to_lowercase());

        // 2. Extract company/organization names
        for cap in self.company_suffix.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let name = m.as_str().to_string();
                let key = name.to_lowercase();
                if !seen_entities.contains(&key) {
                    seen_entities.insert(key);
                    entities.push(ExtractedEntity {
                        name: name.clone(),
                        entity_type: "Organization".to_string(),
                        description: None,
                    });
                    // Speaker mentioned this organization
                    relations.push(ExtractedRelation {
                        source: speaker_name.clone(),
                        target: name,
                        relation_type: "mentioned".to_string(),
                        detail: None,
                    });
                }
            }
        }

        // 3. Extract location mentions
        for cap in self.location_patterns.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let name = m.as_str().to_string();
                let key = name.to_lowercase();
                if !seen_entities.contains(&key) && !is_common_word(&name) {
                    seen_entities.insert(key);
                    entities.push(ExtractedEntity {
                        name: name.clone(),
                        entity_type: "Location".to_string(),
                        description: None,
                    });
                    relations.push(ExtractedRelation {
                        source: speaker_name.clone(),
                        target: name,
                        relation_type: "mentioned".to_string(),
                        detail: None,
                    });
                }
            }
        }

        // 4. Extract capitalized phrases (potential person/org names)
        for cap in self.capitalized_phrase.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let name = m.as_str().to_string();
                let key = name.to_lowercase();
                if !seen_entities.contains(&key) && !is_common_phrase(&name) {
                    seen_entities.insert(key);
                    // Guess type: if 2 words and no company suffix, likely a person
                    let word_count = name.split_whitespace().count();
                    let entity_type = if word_count == 2 {
                        "Person"
                    } else {
                        "Organization"
                    };
                    entities.push(ExtractedEntity {
                        name: name.clone(),
                        entity_type: entity_type.to_string(),
                        description: None,
                    });
                    relations.push(ExtractedRelation {
                        source: speaker_name.clone(),
                        target: name,
                        relation_type: "mentioned".to_string(),
                        detail: None,
                    });
                }
            }
        }

        // 5. Extract topic keywords from quoted terms
        for cap in self.quote_pattern.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                let name = m.as_str().to_string();
                let key = name.to_lowercase();
                if !seen_entities.contains(&key) && name.len() > 2 {
                    seen_entities.insert(key);
                    entities.push(ExtractedEntity {
                        name,
                        entity_type: "Topic".to_string(),
                        description: None,
                    });
                }
            }
        }

        ExtractionResult {
            entities,
            relations,
        }
    }
}

impl Default for RuleBasedExtractor {
    fn default() -> Self {
        Self::new()
    }
}

/// Common words that should NOT be treated as entities even if capitalized.
fn is_common_word(word: &str) -> bool {
    const COMMON: &[&str] = &[
        "The",
        "This",
        "That",
        "These",
        "Those",
        "There",
        "Here",
        "What",
        "Which",
        "Where",
        "When",
        "Who",
        "How",
        "Why",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Today",
        "Tomorrow",
        "Yesterday",
        "Well",
        "Also",
        "Just",
        "Still",
        "Even",
        "Really",
        "Actually",
        "Please",
        "Thank",
        "Thanks",
        "Sorry",
        "Hello",
        "Hey",
    ];
    COMMON.iter().any(|&c| c == word)
}

/// Common capitalized phrases that are NOT entity names.
fn is_common_phrase(phrase: &str) -> bool {
    const COMMON: &[&str] = &[
        "Good Morning",
        "Good Afternoon",
        "Good Evening",
        "Thank You",
        "By The Way",
        "For Example",
        "In Addition",
        "On The Other Hand",
    ];
    COMMON.iter().any(|&c| c.eq_ignore_ascii_case(phrase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_speaker_always_person() {
        let extractor = RuleBasedExtractor::new();
        let result = extractor.extract("Speaker 1", "hello world");
        assert!(
            result
                .entities
                .iter()
                .any(|e| e.name == "Speaker 1" && e.entity_type == "Person"),
            "Speaker should always be added as a Person entity"
        );
    }

    #[test]
    fn test_extract_organization() {
        let extractor = RuleBasedExtractor::new();
        let result = extractor.extract("Speaker 1", "I work at Acme Corp and they are great");
        assert!(
            result
                .entities
                .iter()
                .any(|e| e.entity_type == "Organization"),
            "Should extract an Organization from 'Acme Corp'"
        );
    }

    #[test]
    fn test_extract_capitalized_names() {
        let extractor = RuleBasedExtractor::new();
        let result = extractor.extract("Speaker 1", "I talked to John Smith about the project");
        assert!(
            result
                .entities
                .iter()
                .any(|e| e.name == "John Smith" && e.entity_type == "Person"),
            "Should extract 'John Smith' as a Person"
        );
    }

    #[test]
    fn test_extract_location() {
        let extractor = RuleBasedExtractor::new();
        let result = extractor.extract("Speaker 1", "The office is based in San Francisco");
        assert!(
            result.entities.iter().any(|e| e.entity_type == "Location"),
            "Should extract a Location from 'based in San Francisco'"
        );
    }

    #[test]
    fn test_relations_link_speaker_to_entities() {
        let extractor = RuleBasedExtractor::new();
        let result = extractor.extract("Bob", "I met with Acme Corp yesterday");
        assert!(
            result
                .relations
                .iter()
                .any(|r| r.source == "Bob" && r.relation_type == "mentioned"),
            "Should have a 'mentioned' relation from speaker to extracted entity"
        );
    }

    #[test]
    fn test_no_duplicate_entities() {
        let extractor = RuleBasedExtractor::new();
        let result = extractor.extract("Speaker 1", "John Smith met John Smith again");
        let john_count = result
            .entities
            .iter()
            .filter(|e| e.name == "John Smith")
            .count();
        assert_eq!(john_count, 1, "Should not duplicate entities");
    }

    #[test]
    fn test_quoted_topics() {
        let extractor = RuleBasedExtractor::new();
        let result = extractor.extract("Speaker 1", "We discussed 'machine learning' today");
        assert!(
            result
                .entities
                .iter()
                .any(|e| e.name == "machine learning" && e.entity_type == "Topic"),
            "Should extract quoted terms as Topic entities"
        );
    }
}
