import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type {
    ApiEndpointConfig,
    AppSettings,
    AudioGraphStore,
    AudioSourceInfo,
    ChatMessage,
    ChatResponse,
    ModelInfo,
    ModelStatus,
    ProcessInfo,
    StageStatus,
} from "../types";

const idleStage: StageStatus = { type: "Idle" };

export const useAudioGraphStore = create<AudioGraphStore>((set, get) => ({
    // ── Audio sources ────────────────────────────────────────────────────
    audioSources: [],
    selectedSourceIds: [],
    setAudioSources: (sources) => set({ audioSources: sources }),
    toggleSourceId: (id) =>
        set((state) => {
            const idx = state.selectedSourceIds.indexOf(id);
            if (idx >= 0) {
                return { selectedSourceIds: state.selectedSourceIds.filter((sid) => sid !== id) };
            }
            return { selectedSourceIds: [...state.selectedSourceIds, id] };
        }),
    clearSelectedSources: () => set({ selectedSourceIds: [] }),
    fetchSources: async () => {
        try {
            const sources = await invoke<AudioSourceInfo[]>("list_audio_sources");
            set({ audioSources: sources, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },

    // ── Processes ────────────────────────────────────────────────────────
    processes: [],
    searchFilter: '',
    fetchProcesses: async () => {
        try {
            const processes = await invoke<ProcessInfo[]>("list_running_processes");
            set({ processes });
        } catch (err) {
            console.error("Failed to fetch processes:", err);
        }
    },
    setSearchFilter: (filter: string) => set({ searchFilter: filter }),

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
        const { selectedSourceIds } = get();
        if (selectedSourceIds.length === 0) {
            set({ error: "No audio source selected" });
            return;
        }
        try {
            for (const sourceId of selectedSourceIds) {
                await invoke("start_capture", { sourceId });
            }
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
        const { selectedSourceIds } = get();
        if (selectedSourceIds.length === 0) return;
        try {
            for (const sourceId of selectedSourceIds) {
                await invoke("stop_capture", { sourceId });
            }
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

    // ── Models ────────────────────────────────────────────────────────────
    models: [],
    isDownloading: false,
    downloadProgress: null,
    fetchModels: async () => {
        try {
            const models = await invoke<ModelInfo[]>("list_available_models");
            set({ models, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    downloadModel: async (filename: string) => {
        set({ isDownloading: true, downloadProgress: null });
        try {
            await invoke("download_model_cmd", { modelFilename: filename });
            // Refresh model list after download
            const models = await invoke<ModelInfo[]>("list_available_models");
            set({ models, isDownloading: false, error: null });
        } catch (e) {
            set({
                isDownloading: false,
                error: e instanceof Error ? e.message : String(e),
            });
        }
    },

    // ── API endpoint ──────────────────────────────────────────────────────
    apiConfig: null,
    configureApiEndpoint: async (config: ApiEndpointConfig) => {
        try {
            await invoke("configure_api_endpoint", {
                endpoint: config.endpoint,
                apiKey: config.apiKey ?? null,
                model: config.model,
            });
            set({ apiConfig: config, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    clearApiEndpoint: () => set({ apiConfig: null }),

    // ── Settings ──────────────────────────────────────────────────────────
    settings: null,
    modelStatus: null,
    settingsOpen: false,
    settingsLoading: false,
    isDeletingModel: null,

    openSettings: () => {
        set({ settingsOpen: true });
        const { fetchSettings, fetchModels, fetchModelStatus } = get();
        fetchSettings();
        fetchModels();
        fetchModelStatus();
    },
    closeSettings: () => set({ settingsOpen: false }),

    fetchSettings: async () => {
        set({ settingsLoading: true });
        try {
            const settings = await invoke<AppSettings>("load_settings_cmd");
            set({ settings, settingsLoading: false, error: null });
        } catch (e) {
            set({
                settingsLoading: false,
                error: e instanceof Error ? e.message : String(e),
            });
        }
    },
    saveSettings: async (settings: AppSettings) => {
        try {
            await invoke("save_settings_cmd", { settings });
            set({ settings, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    fetchModelStatus: async () => {
        try {
            const modelStatus = await invoke<ModelStatus>("get_model_status");
            set({ modelStatus, error: null });
        } catch (e) {
            set({ error: e instanceof Error ? e.message : String(e) });
        }
    },
    deleteModel: async (filename: string) => {
        set({ isDeletingModel: filename });
        try {
            await invoke("delete_model_cmd", { modelFilename: filename });
            // Refresh models and model status after deletion
            const models = await invoke<ModelInfo[]>("list_available_models");
            const modelStatus = await invoke<ModelStatus>("get_model_status");
            set({ models, modelStatus, isDeletingModel: null, error: null });
        } catch (e) {
            set({
                isDeletingModel: null,
                error: e instanceof Error ? e.message : String(e),
            });
        }
    },
}));
