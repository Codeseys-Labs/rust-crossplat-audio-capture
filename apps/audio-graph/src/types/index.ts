// Type aliases
export type SourceId = string;
export type SegmentId = string;

// Audio source types
export type AudioSourceType =
    | { type: "SystemDefault" }
    | { type: "Device"; device_id: string }
    | { type: "Application"; pid: number; app_name: string };

export interface AudioSourceInfo {
    id: SourceId;
    name: string;
    source_type: AudioSourceType;
    is_active: boolean;
}

// Transcript types
export interface TranscriptSegment {
    id: string; // UUID
    source_id: SourceId;
    speaker_id: string | null;
    speaker_label: string | null;
    text: string;
    start_time: number; // seconds since capture start
    end_time: number;
    confidence: number;
}

// Knowledge graph types
export interface GraphEntity {
    id: string;
    name: string;
    entity_type: string; // PERSON, ORG, LOCATION, EVENT, CONCEPT
    mention_count: number;
    first_seen: number;
    last_seen: number;
    aliases: string[];
}

export interface GraphRelation {
    id: string;
    source_id: string; // source entity ID
    target_id: string; // target entity ID
    relation_type: string; // WORKS_AT, LOCATED_IN, KNOWS, etc.
    valid_from: number;
    valid_until: number | null;
    confidence: number;
    source_segment_id: string;
}

export interface GraphSnapshot {
    entities: GraphEntity[];
    relations: GraphRelation[];
    last_updated: number;
    node_count: number;
    edge_count: number;
}

// Pipeline status types
export type StageStatus =
    | { type: "Idle" }
    | { type: "Running"; processed_count: number }
    | { type: "Error"; message: string };

export type SidecarStatus =
    | { type: "NotStarted" }
    | { type: "Starting" }
    | { type: "Healthy" }
    | { type: "Unhealthy"; reason: string }
    | { type: "Stopped" };

export interface PipelineStatus {
    capture: StageStatus;
    pipeline: StageStatus;
    asr: StageStatus;
    diarization: StageStatus;
    entity_extraction: StageStatus;
    graph: StageStatus;
    sidecar: SidecarStatus;
}

// Speaker types
export interface SpeakerInfo {
    id: string;
    label: string;
    color: string; // hex color for UI
    total_speaking_time: number; // seconds
    segment_count: number;
}

// Capture configuration
export interface CaptureSessionConfig {
    source_id: SourceId;
    sample_rate?: number;
    channels?: number;
}

// Event payloads
export interface CaptureErrorPayload {
    source_id: string;
    error: string;
    recoverable: boolean;
}
