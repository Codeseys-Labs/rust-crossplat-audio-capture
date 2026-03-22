/**
 * Shared formatting utilities for the AudioGraph frontend.
 *
 * Consolidates duplicated `formatTime` / `formatDuration` helpers that
 * previously lived in SpeakerPanel, KnowledgeGraphViewer, and LiveTranscript.
 */

/** Format seconds as `M:SS` (e.g. `"2:05"`). */
export function formatTime(seconds: number): string {
    if (!seconds && seconds !== 0) return "—";
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return mins > 0 ? `${mins}:${secs.toString().padStart(2, "0")}` : `0:${secs.toString().padStart(2, "0")}`;
}

/** Format seconds as a human-friendly duration string (e.g. `"3m 12s"`). */
export function formatDuration(seconds: number): string {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}m ${secs}s`;
}
