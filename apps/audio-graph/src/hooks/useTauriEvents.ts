import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAudioGraphStore } from "../store";
import type {
    TranscriptSegment,
    GraphSnapshot,
    PipelineStatus,
    SpeakerInfo,
    CaptureErrorPayload,
} from "../types";

// Event name constants — must match src-tauri/src/events.rs
const TRANSCRIPT_UPDATE = "transcript-update";
const GRAPH_UPDATE = "graph-update";
const PIPELINE_STATUS = "pipeline-status";
const SPEAKER_DETECTED = "speaker-detected";
const CAPTURE_ERROR = "capture-error";

/**
 * Hook that subscribes to all Tauri backend events and updates the Zustand store.
 * Should be called once at the app root level.
 */
export function useTauriEvents(): void {
    const addTranscriptSegment = useAudioGraphStore((s) => s.addTranscriptSegment);
    const setGraphSnapshot = useAudioGraphStore((s) => s.setGraphSnapshot);
    const setPipelineStatus = useAudioGraphStore((s) => s.setPipelineStatus);
    const addOrUpdateSpeaker = useAudioGraphStore((s) => s.addOrUpdateSpeaker);
    const setError = useAudioGraphStore((s) => s.setError);

    useEffect(() => {
        const unlisten: Array<() => void> = [];

        async function setup() {
            unlisten.push(
                await listen<TranscriptSegment>(TRANSCRIPT_UPDATE, (event) => {
                    addTranscriptSegment(event.payload);
                }),
            );

            unlisten.push(
                await listen<GraphSnapshot>(GRAPH_UPDATE, (event) => {
                    setGraphSnapshot(event.payload);
                }),
            );

            unlisten.push(
                await listen<PipelineStatus>(PIPELINE_STATUS, (event) => {
                    setPipelineStatus(event.payload);
                }),
            );

            unlisten.push(
                await listen<SpeakerInfo>(SPEAKER_DETECTED, (event) => {
                    addOrUpdateSpeaker(event.payload);
                }),
            );

            unlisten.push(
                await listen<CaptureErrorPayload>(CAPTURE_ERROR, (event) => {
                    console.error("Capture error:", event.payload);
                    setError(event.payload.error);
                }),
            );
        }

        setup();

        return () => {
            unlisten.forEach((fn) => fn());
        };
    }, [addTranscriptSegment, setGraphSnapshot, setPipelineStatus, addOrUpdateSpeaker, setError]);
}
