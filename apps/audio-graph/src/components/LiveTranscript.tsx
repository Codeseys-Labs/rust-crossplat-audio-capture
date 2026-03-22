import { useRef, useEffect, useMemo, useCallback } from "react";
import { useAudioGraphStore } from "../store";
import { formatTime } from "../utils/format";

/** Default fallback colors when speaker has no assigned color. */
const FALLBACK_COLORS = [
  "#60a5fa",
  "#f59e0b",
  "#10b981",
  "#ef4444",
  "#a78bfa",
  "#ec4899",
  "#6b7280",
];

function LiveTranscript() {
  const segments = useAudioGraphStore((s) => s.transcriptSegments);
  const speakers = useAudioGraphStore((s) => s.speakers);

  const scrollRef = useRef<HTMLDivElement>(null);
  const wasNearBottomRef = useRef(true);

  // Build a quick speaker-color lookup
  const speakerColorMap = useMemo(() => {
    const map = new Map<string, string>();
    speakers.forEach((s) => {
      map.set(s.id, s.color);
    });
    return map;
  }, [speakers]);

  // Get color for a speaker, with fallback
  const getSpeakerColor = useCallback(
    (speakerId: string | null): string => {
      if (!speakerId) return FALLBACK_COLORS[0];
      const mapped = speakerColorMap.get(speakerId);
      if (mapped) return mapped;
      // Deterministic fallback based on id hash
      let hash = 0;
      for (let i = 0; i < speakerId.length; i++) {
        hash = (hash * 31 + speakerId.charCodeAt(i)) | 0;
      }
      return FALLBACK_COLORS[Math.abs(hash) % FALLBACK_COLORS.length];
    },
    [speakerColorMap]
  );

  // Auto-scroll: only if user is near the bottom
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;

    // Check if we were near the bottom before the new segment arrived
    if (wasNearBottomRef.current) {
      el.scrollTop = el.scrollHeight;
    }
  }, [segments]);

  // Track scroll position to decide auto-scroll behavior
  const handleScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const distanceFromBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight;
    wasNearBottomRef.current = distanceFromBottom < 100;
  }, []);

  // Display last 200 segments for performance
  const visibleSegments = useMemo(
    () => segments.slice(-200),
    [segments]
  );

  return (
    <div className="transcript">
      <div className="transcript__header">
        <h3 className="panel-title">Live Transcript</h3>
        {segments.length > 0 && (
          <span className="transcript__count">{segments.length}</span>
        )}
      </div>

      <div
        className="transcript__list"
        ref={scrollRef}
        onScroll={handleScroll}
        role="log"
        aria-live="polite"
        aria-label="Live transcript"
      >
        {visibleSegments.length === 0 ? (
          <div className="transcript__empty">
            <span className="transcript__empty-icon" aria-hidden="true">
              ═══
            </span>
            <p className="transcript__empty-text">Waiting for speech…</p>
          </div>
        ) : (
          visibleSegments.map((seg) => (
            <div key={seg.id} className="transcript__segment">
              <div className="transcript__segment-header">
                {seg.speaker_label && (
                  <span
                    className="transcript__speaker-badge"
                    style={{
                      backgroundColor: `${getSpeakerColor(seg.speaker_id)}20`,
                      color: getSpeakerColor(seg.speaker_id),
                      borderColor: `${getSpeakerColor(seg.speaker_id)}40`,
                    }}
                  >
                    {seg.speaker_label}
                  </span>
                )}
                <span className="transcript__timestamp">
                  {formatTime(seg.start_time)}
                </span>
              </div>
              <p className="transcript__text">{seg.text}</p>
              {seg.confidence < 1 && (
                <div
                  className="transcript__confidence"
                  role="meter"
                  aria-valuenow={Math.round(seg.confidence * 100)}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  aria-label={`Confidence: ${Math.round(seg.confidence * 100)}%`}
                >
                  <div
                    className="transcript__confidence-fill"
                    style={{ width: `${seg.confidence * 100}%` }}
                  />
                </div>
              )}
            </div>
          ))
        )}
      </div>
    </div>
  );
}

export default LiveTranscript;
