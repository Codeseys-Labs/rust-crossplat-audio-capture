import { useEffect } from "react";
import AudioSourceSelector from "./components/AudioSourceSelector";
import LiveTranscript from "./components/LiveTranscript";
import KnowledgeGraphViewer from "./components/KnowledgeGraphViewer";
import ControlBar from "./components/ControlBar";
import SpeakerPanel from "./components/SpeakerPanel";
import PipelineStatusBar from "./components/PipelineStatusBar";
import { useTauriEvents } from "./hooks/useTauriEvents";
import "./App.css";

function App() {
  // Subscribe to Tauri backend events
  useTauriEvents();

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
          <LiveTranscript />
        </aside>
      </div>
      <PipelineStatusBar />
    </div>
  );
}

export default App;
