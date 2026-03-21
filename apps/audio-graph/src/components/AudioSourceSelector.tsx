import { useAudioGraphStore } from "../store";

function AudioSourceSelector() {
  const audioSources = useAudioGraphStore((s) => s.audioSources);
  const selectedSourceId = useAudioGraphStore((s) => s.selectedSourceId);
  const setSelectedSourceId = useAudioGraphStore((s) => s.setSelectedSourceId);

  return (
    <div className="panel audio-source-selector">
      <h3 className="panel-title">Audio Sources</h3>
      {audioSources.length === 0 ? (
        <p className="panel-empty">No audio sources detected</p>
      ) : (
        <ul className="source-list">
          {audioSources.map((source) => (
            <li
              key={source.id}
              className={`source-item ${selectedSourceId === source.id ? "selected" : ""}`}
              onClick={() => setSelectedSourceId(source.id)}
            >
              <span className={`source-indicator ${source.is_active ? "active" : ""}`} />
              <span className="source-name">{source.name}</span>
              <span className="source-type">{source.source_type.type}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export default AudioSourceSelector;
