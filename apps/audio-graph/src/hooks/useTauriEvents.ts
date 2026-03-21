import { useEffect } from "react";
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

    useEffect(() => {
        // TODO: Subscribe to Tauri events once @tauri-apps/api is installed
        // Example:
        // import { listen } from "@tauri-apps/api/event";
        //
        // const unlisteners = Promise.all([
        //   listen<TranscriptSegment>(TRANSCRIPT_UPDATE, (event) => {
        //     addTranscriptSegment(event.payload);
        //   }),
        //   listen<GraphSnapshot>(GRAPH_UPDATE, (event) => {
        //     setGraphSnapshot(event.payload);
        //   }),
        //   listen<PipelineStatus>(PIPELINE_STATUS, (event) => {
        //     setPipelineStatus(event.payload);
        //   }),
        //   listen<SpeakerInfo>(SPEAKER_DETECTED, (event) => {
        //     addOrUpdateSpeaker(event.payload);
        //   }),
        //   listen<CaptureErrorPayload>(CAPTURE_ERROR, (event) => {
        //     console.error("Capture error:", event.payload);
        //   }),
        // ]);
        //
        // return () => {
        //   unlisteners.then((fns) => fns.forEach((fn) => fn()));
        // };

        console.log("Tauri event subscriptions will be initialized here");
    }, [addTranscriptSegment, setGraphSnapshot, setPipelineStatus, addOrUpdateSpeaker]);
}
