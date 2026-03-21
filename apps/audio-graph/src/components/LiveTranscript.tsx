import { useRef, useEffect } from "react";
import { useAudioGraphStore } from "../store";

function LiveTranscript() {
  const segments = useAudioGraphStore((s) => s.transcriptSegments);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [segments]);

  const formatTime = (seconds: number): string => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins.toString().padStart(2, "0")}:${secs.toString().padStart(2, "0")}`;
  };

  return (
    <div className="panel live-transcript">
      <h3 className="panel-title">Live Transcript</h3>
      <div className="transcript-scroll" ref={scrollRef}>
        {segments.length === 0 ? (
          <p className="panel-empty">Waiting for speech...</p>
        ) : (
          segments.map((seg) => (
            <div key={seg.id} className="transcript-entry">
              <span className="transcript-time">{formatTime(seg.start_time)}</span>
              {seg.speaker_label && (
                <span className="transcript-speaker">{seg.speaker_label}</span>
              )}
              <span className="transcript-text">{seg.text}</span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

export default LiveTranscript;
