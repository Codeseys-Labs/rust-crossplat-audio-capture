import { useAudioGraphStore } from "../store";

function SpeakerPanel() {
  const speakers = useAudioGraphStore((s) => s.speakers);

  const formatDuration = (seconds: number): string => {
    const mins = Math.floor(seconds / 60);
    const secs = Math.floor(seconds % 60);
    return `${mins}m ${secs}s`;
  };

  return (
    <div className="panel speaker-panel">
      <h3 className="panel-title">Speakers</h3>
      {speakers.length === 0 ? (
        <p className="panel-empty">No speakers detected yet</p>
      ) : (
        <ul className="speaker-list">
          {speakers.map((speaker) => (
            <li key={speaker.id} className="speaker-item">
              <span
                className="speaker-color"
                style={{ backgroundColor: speaker.color }}
              />
              <div className="speaker-info">
                <span className="speaker-label">{speaker.label}</span>
                <span className="speaker-stats">
                  {formatDuration(speaker.total_speaking_time)} · {speaker.segment_count} segments
                </span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export default SpeakerPanel;
