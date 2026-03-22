import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
    AudioGraphStore,
    AudioSourceInfo,
    ChatMessage,
    ChatResponse,
    StageStatus,
    SidecarStatus,
} from "../types";

const idleStage: StageStatus = { type: "Idle" };
const defaultSidecar: SidecarStatus = { type: "NotStarted" };

export const useAudioGraphStore = create<AudioGraphStore>((set, get) => ({
    // ── Audio sources ────────────────────────────────────────────────────
    audioSources: [],
    selectedSourceId: null,
    setAudioSources: (sources) => set({ audioSources: sources }),
    setSelectedSourceId: (id) => set({ selectedSourceId: id }),
    fetchSources: async () => {
        try {
            const sources = await invoke<AudioSourceInfo[]>("list_audio_sources");
            set({ audioSources: sources, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Transcript ───────────────────────────────────────────────────────
    transcriptSegments: [],
    addTranscriptSegment: (segment) =>
        set((state) => ({
            transcriptSegments: [...state.transcriptSegments.slice(-499), segment],
        })),
    clearTranscript: () => set({ transcriptSegments: [] }),

    // ── Knowledge graph ──────────────────────────────────────────────────
    graphSnapshot: {
        nodes: [],
        links: [],
        stats: { total_nodes: 0, total_edges: 0, total_episodes: 0 },
    },
    setGraphSnapshot: (snapshot) => set({ graphSnapshot: snapshot }),

    // ── Pipeline status ──────────────────────────────────────────────────
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

    // ── Speakers ─────────────────────────────────────────────────────────
    speakers: [],
    addOrUpdateSpeaker: (speaker) =>
        set((state) => {
            const idx = state.speakers.findIndex((s) => s.id === speaker.id);
            if (idx >= 0) {
                const updated = [...state.speakers];
                updated[idx] = speaker;
                return { speakers: updated };
            }
            return { speakers: [...state.speakers, speaker] };
        }),
    clearSpeakers: () => set({ speakers: [] }),

    // ── Capture state ────────────────────────────────────────────────────
    isCapturing: false,
    captureStartTime: null,
    setIsCapturing: (capturing) => set({ isCapturing: capturing }),
    startCapture: async () => {
        const { selectedSourceId } = get();
        if (!selectedSourceId) {
            set({ error: "No audio source selected" });
            return;
        }
        try {
            await invoke("start_capture", { sourceId: selectedSourceId });
            set({
                isCapturing: true,
                captureStartTime: Date.now(),
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    stopCapture: async () => {
        const { selectedSourceId } = get();
        if (!selectedSourceId) return;
        try {
            await invoke("stop_capture", { sourceId: selectedSourceId });
            set({
                isCapturing: false,
                captureStartTime: null,
                error: null,
            });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Error state ──────────────────────────────────────────────────────
    error: null,
    setError: (error) => set({ error }),
    clearError: () => set({ error: null }),

    // ── Chat ─────────────────────────────────────────────────────────────
    chatMessages: [],
    isChatLoading: false,
    rightPanelTab: "transcript",
    setRightPanelTab: (tab) => set({ rightPanelTab: tab }),
    sendChatMessage: async (message: string) => {
        // Add user message immediately for responsiveness
        const userMsg: ChatMessage = { role: "user", content: message };
        set((state) => ({
            chatMessages: [...state.chatMessages, userMsg],
            isChatLoading: true,
        }));

        try {
            const response = await invoke<ChatResponse>("send_chat_message", { message });
            set((state) => ({
                chatMessages: [...state.chatMessages, response.message],
                isChatLoading: false,
            }));
        } catch (e) {
            // Add error as assistant message
            const errorMsg: ChatMessage = {
                role: "assistant",
                content: `Error: ${e instanceof Error ? e.message : String(e)}`,
            };
            set((state) => ({
                chatMessages: [...state.chatMessages, errorMsg],
                isChatLoading: false,
            }));
        }
    },
    clearChatHistory: async () => {
        try {
            await invoke("clear_chat_history");
            set({ chatMessages: [] });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
}));
