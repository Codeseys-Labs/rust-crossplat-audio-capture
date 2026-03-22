import { useState, useEffect, useCallback } from "react";
import { useAudioGraphStore } from "../store";

function ControlBar() {
  const isCapturing = useAudioGraphStore((s) => s.isCapturing);
  const selectedSourceId = useAudioGraphStore((s) => s.selectedSourceId);
  const audioSources = useAudioGraphStore((s) => s.audioSources);
  const captureStartTime = useAudioGraphStore((s) => s.captureStartTime);
  const startCapture = useAudioGraphStore((s) => s.startCapture);
  const stopCapture = useAudioGraphStore((s) => s.stopCapture);

  const [elapsed, setElapsed] = useState("00:00");

  // Update elapsed timer every second while capturing
  useEffect(() => {
    if (!isCapturing || captureStartTime === null) {
      setElapsed("00:00");
      return;
    }

    const tick = () => {
      const diff = Math.floor((Date.now() - captureStartTime) / 1000);
      const mins = Math.floor(diff / 60)
        .toString()
        .padStart(2, "0");
      const secs = (diff % 60).toString().padStart(2, "0");
      setElapsed(`${mins}:${secs}`);
    };

    tick(); // Immediate first tick
    const interval = setInterval(tick, 1000);
    return () => clearInterval(interval);
  }, [isCapturing, captureStartTime]);

  const handleToggleCapture = useCallback(async () => {
    if (isCapturing) {
      await stopCapture();
    } else {
      await startCapture();
    }
  }, [isCapturing, startCapture, stopCapture]);

  // Find selected source name
  const selectedSource = audioSources.find((s) => s.id === selectedSourceId);
  const canStart = selectedSourceId !== null && !isCapturing;

  return (
    <header className="control-bar" role="toolbar" aria-label="Capture controls">
      <div className="control-bar__left">
        <h1 className="control-bar__title">AudioGraph</h1>
      </div>

      <div className="control-bar__center">
        <button
          className={`control-bar__capture-btn ${isCapturing ? "control-bar__capture-btn--stop" : "control-bar__capture-btn--start"}`}
          onClick={handleToggleCapture}
          disabled={!canStart && !isCapturing}
          aria-label={isCapturing ? "Stop capture" : "Start capture"}
        >
          {isCapturing ? "⏹ Stop" : "⏺ Start"}
        </button>

        {isCapturing && (
          <div className="control-bar__recording">
            <span className="control-bar__rec-dot" aria-hidden="true" />
            <span className="control-bar__timer">{elapsed}</span>
          </div>
        )}

        {selectedSource && !isCapturing && (
          <span className="control-bar__source-name" title={selectedSource.name}>
            {selectedSource.name}
          </span>
        )}

        {!selectedSourceId && !isCapturing && (
          <span className="control-bar__hint">Select an audio source to begin</span>
        )}
      </div>

      <div className="control-bar__right">
        {isCapturing && selectedSource && (
          <span className="control-bar__active-source">
            🎧 {selectedSource.name}
          </span>
        )}
      </div>
    </header>
  );
}

export default ControlBar;
