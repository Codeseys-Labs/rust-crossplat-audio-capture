import { useAudioGraphStore } from "../store";
import type { StageStatus, SidecarStatus } from "../types";

function getStageLabel(status: StageStatus): string {
  switch (status.type) {
    case "Idle":
      return "Idle";
    case "Running":
      return `Running (${status.processed_count})`;
    case "Error":
      return `Error: ${status.message}`;
  }
}

function getSidecarLabel(status: SidecarStatus): string {
  switch (status.type) {
    case "NotStarted":
      return "Not Started";
    case "Starting":
      return "Starting...";
    case "Healthy":
      return "Healthy";
    case "Unhealthy":
      return `Unhealthy: ${status.reason}`;
    case "Stopped":
      return "Stopped";
  }
}

function getStatusColor(status: StageStatus): string {
  switch (status.type) {
    case "Idle":
      return "var(--text-muted)";
    case "Running":
      return "var(--accent-green)";
    case "Error":
      return "var(--accent-red)";
  }
}

function PipelineStatusBar() {
  const pipelineStatus = useAudioGraphStore((s) => s.pipelineStatus);

  const stages = [
    { name: "Capture", status: pipelineStatus.capture },
    { name: "Pipeline", status: pipelineStatus.pipeline },
    { name: "ASR", status: pipelineStatus.asr },
    { name: "Diarization", status: pipelineStatus.diarization },
    { name: "Extraction", status: pipelineStatus.entity_extraction },
    { name: "Graph", status: pipelineStatus.graph },
  ];

  return (
    <div className="pipeline-status-bar">
      {stages.map((stage) => (
        <div key={stage.name} className="status-item">
          <span
            className="status-dot"
            style={{ backgroundColor: getStatusColor(stage.status) }}
          />
          <span className="status-label">{stage.name}</span>
          <span className="status-value">{getStageLabel(stage.status)}</span>
        </div>
      ))}
      <div className="status-item sidecar-status">
        <span className="status-label">Sidecar</span>
        <span className="status-value">{getSidecarLabel(pipelineStatus.sidecar)}</span>
      </div>
    </div>
  );
}

export default PipelineStatusBar;
