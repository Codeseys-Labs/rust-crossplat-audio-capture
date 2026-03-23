import AudioSourceSelector from "./components/AudioSourceSelector";
import LiveTranscript from "./components/LiveTranscript";
import ChatSidebar from "./components/ChatSidebar";
import KnowledgeGraphViewer from "./components/KnowledgeGraphViewer";
import ControlBar from "./components/ControlBar";
import SpeakerPanel from "./components/SpeakerPanel";
import PipelineStatusBar from "./components/PipelineStatusBar";
import SettingsPage from "./components/SettingsPage";
import { useTauriEvents } from "./hooks/useTauriEvents";
import { useAudioGraphStore } from "./store";
import "./App.css";

function App() {
  // Subscribe to Tauri backend events
  useTauriEvents();

  const error = useAudioGraphStore((s) => s.error);
  const clearError = useAudioGraphStore((s) => s.clearError);
  const rightPanelTab = useAudioGraphStore((s) => s.rightPanelTab);
  const setRightPanelTab = useAudioGraphStore((s) => s.setRightPanelTab);
  const settingsOpen = useAudioGraphStore((s) => s.settingsOpen);

  return (
    <div className="app-container">
      <ControlBar />
      <div className="main-layout">
        <aside className="left-panel">
          <AudioSourceSelector />
          <SpeakerPanel />
        </aside>
        <main className="center-panel">
          <KnowledgeGraphViewer />
        </main>
        <aside className="right-panel">
          <div className="right-panel__tabs">
            <button
              className={`right-panel__tab ${rightPanelTab === "transcript" ? "right-panel__tab--active" : ""}`}
              onClick={() => setRightPanelTab("transcript")}
            >
              📝 Transcript
            </button>
            <button
              className={`right-panel__tab ${rightPanelTab === "chat" ? "right-panel__tab--active" : ""}`}
              onClick={() => setRightPanelTab("chat")}
            >
              💬 Chat
            </button>
          </div>
          {rightPanelTab === "transcript" ? (
            <LiveTranscript />
          ) : (
            <ChatSidebar />
          )}
        </aside>
      </div>
      <PipelineStatusBar />

      {/* Error toast notification */}
      {error && (
        <div className="error-toast" role="alert">
          <span className="error-toast__icon" aria-hidden="true">
            ⚠️
          </span>
          <span className="error-toast__message">{error}</span>
          <button
            className="error-toast__dismiss"
            onClick={clearError}
            aria-label="Dismiss error"
          >
            ✕
          </button>
        </div>
      )}

      {/* Settings modal */}
      {settingsOpen && <SettingsPage />}
    </div>
  );
}

export default App;
