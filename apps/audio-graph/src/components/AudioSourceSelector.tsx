import { useEffect, useMemo, useCallback } from "react";
import { useAudioGraphStore } from "../store";
import type { AudioSourceInfo } from "../types";

// Group audio sources by type
function getSourceGroup(source: AudioSourceInfo): {
  label: string;
  icon: string;
} {
  switch (source.source_type.type) {
    case "SystemDefault":
      return { label: "System", icon: "🖥️" };
    case "Device":
      return { label: "Devices", icon: "🎤" };
    case "Application":
      return { label: "Applications", icon: "📱" };
    default:
      return { label: "Other", icon: "📦" };
  }
}

// Group ordering for consistent display
const GROUP_ORDER: Record<string, number> = {
  System: 0,
  Devices: 1,
  Applications: 2,
  "Running Processes": 3,
  Other: 4,
};

export default function AudioSourceSelector() {
  const audioSources = useAudioGraphStore((s) => s.audioSources);
  const selectedSourceIds = useAudioGraphStore((s) => s.selectedSourceIds);
  const toggleSourceId = useAudioGraphStore((s) => s.toggleSourceId);
  const fetchSources = useAudioGraphStore((s) => s.fetchSources);
  const isCapturing = useAudioGraphStore((s) => s.isCapturing);
  const processes = useAudioGraphStore((s) => s.processes);
  const searchFilter = useAudioGraphStore((s) => s.searchFilter);
  const setSearchFilter = useAudioGraphStore((s) => s.setSearchFilter);
  const fetchProcesses = useAudioGraphStore((s) => s.fetchProcesses);

  useEffect(() => {
    fetchSources();
    fetchProcesses();
  }, [fetchSources, fetchProcesses]);

  const filterText = searchFilter.toLowerCase().trim();

  // Group and filter audio sources
  const groupedSources = useMemo(() => {
    const groups = new Map<
      string,
      { icon: string; sources: AudioSourceInfo[] }
    >();

    for (const source of audioSources) {
      // Apply search filter
      if (filterText && !source.name.toLowerCase().includes(filterText)) {
        continue;
      }

      const { label, icon } = getSourceGroup(source);
      if (!groups.has(label)) {
        groups.set(label, { icon, sources: [] });
      }
      groups.get(label)!.sources.push(source);
    }

    return new Map(
      [...groups.entries()].sort(
        ([a], [b]) => (GROUP_ORDER[a] ?? 99) - (GROUP_ORDER[b] ?? 99),
      ),
    );
  }, [audioSources, filterText]);

  // Filter processes by search text
  const filteredProcesses = useMemo(() => {
    if (!filterText) return processes;
    return processes.filter(
      (p) =>
        p.name.toLowerCase().includes(filterText) ||
        (p.exe_path && p.exe_path.toLowerCase().includes(filterText)),
    );
  }, [processes, filterText]);

  const handleToggle = useCallback(
    (id: string) => {
      if (!isCapturing) toggleSourceId(id);
    },
    [isCapturing, toggleSourceId],
  );

  const handleRefresh = useCallback(() => {
    fetchSources();
    fetchProcesses();
  }, [fetchSources, fetchProcesses]);

  const isSelected = useCallback(
    (id: string) => selectedSourceIds.includes(id),
    [selectedSourceIds],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent, id: string) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        handleToggle(id);
      }
    },
    [handleToggle],
  );

  const noResults =
    filterText && groupedSources.size === 0 && filteredProcesses.length === 0;

  return (
    <div className="audio-source-selector">
      <div className="audio-source-selector__header">
        <span className="audio-source-selector__title">Audio Sources</span>
        <button
          className="audio-source-selector__refresh"
          onClick={handleRefresh}
          disabled={isCapturing}
          title="Refresh sources"
        >
          🔄
        </button>
      </div>

      {/* Search input */}
      <div className="audio-source-selector__search">
        <input
          type="text"
          className="audio-source-selector__search-input"
          placeholder="Search sources & processes..."
          value={searchFilter}
          onChange={(e) => setSearchFilter(e.target.value)}
        />
        {searchFilter && (
          <button
            className="audio-source-selector__search-clear"
            onClick={() => setSearchFilter("")}
            title="Clear search"
          >
            ✕
          </button>
        )}
      </div>

      {audioSources.length === 0 && processes.length === 0 ? (
        <div className="audio-source-selector__empty">
          <p>No audio sources detected</p>
          <button onClick={handleRefresh}>Retry</button>
        </div>
      ) : noResults ? (
        <div className="audio-source-selector__empty">
          <p>No matches for "{searchFilter}"</p>
        </div>
      ) : (
        <div className="audio-source-selector__groups">
          {/* Audio Source Groups (System, Devices, Applications) */}
          {[...groupedSources.entries()].map(([label, { icon, sources }]) => (
            <div key={label}>
              <div className="audio-source-selector__group-label">
                {icon} {label}
              </div>
              <ul className="source-list">
                {sources.map((source) => {
                  const selected = isSelected(source.id);
                  return (
                    <li
                      key={source.id}
                      className={`source-item ${selected ? "source-item--selected" : ""} ${isCapturing ? "source-item--disabled" : ""}`}
                      onClick={() => handleToggle(source.id)}
                      onKeyDown={(e) => handleKeyDown(e, source.id)}
                      role="checkbox"
                      aria-checked={selected}
                      tabIndex={0}
                    >
                      <span
                        className={`source-item__checkbox ${selected ? "source-item__checkbox--checked" : ""}`}
                      />
                      <span className="source-item__name">{source.name}</span>
                      {source.source_type.type === "SystemDefault" && (
                        <span className="source-item__badge">Default</span>
                      )}
                      {selected && (
                        <span className="source-item__check">✓</span>
                      )}
                    </li>
                  );
                })}
              </ul>
            </div>
          ))}

          {/* Running Processes Section */}
          {filteredProcesses.length > 0 && (
            <div>
              <div className="audio-source-selector__group-label">
                🖥️ Running Processes
                <span className="audio-source-selector__group-count">
                  {filteredProcesses.length}
                </span>
              </div>
              <ul className="source-list">
                {filteredProcesses.map((proc) => {
                  const processId = `app:${proc.pid}`;
                  const selected = isSelected(processId);
                  return (
                    <li
                      key={proc.pid}
                      className={`source-item ${selected ? "source-item--selected" : ""} ${isCapturing ? "source-item--disabled" : ""}`}
                      onClick={() => handleToggle(processId)}
                      onKeyDown={(e) => handleKeyDown(e, processId)}
                      role="checkbox"
                      aria-checked={selected}
                      tabIndex={0}
                    >
                      <span
                        className={`source-item__checkbox ${selected ? "source-item__checkbox--checked" : ""}`}
                      />
                      <span className="source-item__name">{proc.name}</span>
                      <span className="source-item__pid">PID {proc.pid}</span>
                      {selected && (
                        <span className="source-item__check">✓</span>
                      )}
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
