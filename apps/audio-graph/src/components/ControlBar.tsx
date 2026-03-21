import { useAudioGraphStore } from "../store";

function ControlBar() {
  const isCapturing = useAudioGraphStore((s) => s.isCapturing);
  const selectedSourceId = useAudioGraphStore((s) => s.selectedSourceId);

  const handleToggleCapture = async () => {
    // TODO: Invoke Tauri commands start_capture / stop_capture
    console.log(isCapturing ? "Stopping capture..." : "Starting capture...", selectedSourceId);
  };

  return (
    <div className="control-bar">
      <div className="control-bar-left">
        <h1 className="app-title">AudioGraph</h1>
      </div>
      <div className="control-bar-center">
        <button
          className={`capture-button ${isCapturing ? "capturing" : ""}`}
          onClick={handleToggleCapture}
          disabled={!selectedSourceId && !isCapturing}
        >
          {isCapturing ? "⏹ Stop" : "⏺ Capture"}
        </button>
      </div>
      <div className="control-bar-right">
        {/* Pipeline indicators will go here */}
      </div>
    </div>
  );
}

export default ControlBar;
