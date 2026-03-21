import { create } from "zustand";
import type {
    AudioSourceInfo,
    TranscriptSegment,
    GraphSnapshot,
    PipelineStatus,
    SpeakerInfo,
    StageStatus,
    SidecarStatus,
} from "../types";

const idleStage: StageStatus = { type: "Idle" };
const defaultSidecar: SidecarStatus = { type: "NotStarted" };

interface AudioGraphStore {
    // Audio sources
    audioSources: AudioSourceInfo[];
    selectedSourceId: string | null;
    setAudioSources: (sources: AudioSourceInfo[]) => void;
    setSelectedSourceId: (id: string | null) => void;

    // Transcript
    transcriptSegments: TranscriptSegment[];
    addTranscriptSegment: (segment: TranscriptSegment) => void;
    clearTranscript: () => void;

    // Knowledge graph
    graphSnapshot: GraphSnapshot;
    setGraphSnapshot: (snapshot: GraphSnapshot) => void;

    // Pipeline status
    pipelineStatus: PipelineStatus;
    setPipelineStatus: (status: PipelineStatus) => void;

    // Speakers
    speakers: SpeakerInfo[];
    addOrUpdateSpeaker: (speaker: SpeakerInfo) => void;
    clearSpeakers: () => void;

    // Capture state
    isCapturing: boolean;
    setIsCapturing: (capturing: boolean) => void;
}

export const useAudioGraphStore = create<AudioGraphStore>((set) => ({
    // Audio sources
    audioSources: [],
    selectedSourceId: null,
    setAudioSources: (sources) => set({ audioSources: sources }),
    setSelectedSourceId: (id) => set({ selectedSourceId: id }),

    // Transcript
    transcriptSegments: [],
    addTranscriptSegment: (segment) =>
        set((state) => ({
            transcriptSegments: [...state.transcriptSegments.slice(-499), segment],
        })),
    clearTranscript: () => set({ transcriptSegments: [] }),

    // Knowledge graph
    graphSnapshot: {
        entities: [],
        relations: [],
        last_updated: 0,
        node_count: 0,
        edge_count: 0,
    },
    setGraphSnapshot: (snapshot) => set({ graphSnapshot: snapshot }),

    // Pipeline status
    pipelineStatus: {
        capture: idleStage,
        pipeline: idleStage,
        asr: idleStage,
        diarization: idleStage,
        entity_extraction: idleStage,
        graph: idleStage,
        sidecar: defaultSidecar,
    },
    setPipelineStatus: (status) => set({ pipelineStatus: status }),

    // Speakers
    speakers: [],
    addOrUpdateSpeaker: (speaker) =>
        set((state) => {
            const existing = state.speakers.findIndex((s) => s.id === speaker.id);
            if (existing >= 0) {
                const updated = [...state.speakers];
                updated[existing] = speaker;
                return { speakers: updated };
            }
            return { speakers: [...state.speakers, speaker] };
        }),
    clearSpeakers: () => set({ speakers: [] }),

    // Capture state
    isCapturing: false,
    setIsCapturing: (capturing) => set({ isCapturing: capturing }),
}));
