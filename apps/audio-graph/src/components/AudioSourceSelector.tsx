import { useEffect, useMemo, useCallback } from "react";
import { useAudioGraphStore } from "../store";
import type { AudioSourceInfo } from "../types";

/** Map source_type discriminant to a display group + icon. */
function getSourceGroup(source: AudioSourceInfo): {
  group: string;
  icon: string;
} {
  switch (source.source_type.type) {
    case "SystemDefault":
      return { group: "System", icon: "🖥️" };
    case "Device":
      return { group: "Devices", icon: "🎤" };
    case "Application":
      return { group: "Applications", icon: "📱" };
    default:
      return { group: "Other", icon: "🔊" };
  }
}

/** Stable ordering for groups. */
const GROUP_ORDER: Record<string, number> = {
  System: 0,
  Devices: 1,
  Applications: 2,
  Other: 3,
};

function AudioSourceSelector() {
  const audioSources = useAudioGraphStore((s) => s.audioSources);
  const selectedSourceIds = useAudioGraphStore((s) => s.selectedSourceIds);
  const toggleSourceId = useAudioGraphStore((s) => s.toggleSourceId);
  const fetchSources = useAudioGraphStore((s) => s.fetchSources);
  const isCapturing = useAudioGraphStore((s) => s.isCapturing);

  // Fetch sources on mount
  useEffect(() => {
    fetchSources();
  }, [fetchSources]);

  // Group sources by type
  const grouped = useMemo(() => {
    const groups = new Map<
      string,
      { icon: string; sources: AudioSourceInfo[] }
    >();

    for (const source of audioSources) {
      const { group, icon } = getSourceGroup(source);
      if (!groups.has(group)) {
        groups.set(group, { icon, sources: [] });
      }
      groups.get(group)!.sources.push(source);
    }

    // Sort groups by predefined order
    return Array.from(groups.entries()).sort(
      ([a], [b]) => (GROUP_ORDER[a] ?? 99) - (GROUP_ORDER[b] ?? 99),
    );
  }, [audioSources]);

  const handleSelect = useCallback(
    (id: string) => {
      if (!isCapturing) {
        toggleSourceId(id);
      }
    },
    [isCapturing, toggleSourceId],
  );

  const handleRefresh = useCallback(() => {
    fetchSources();
  }, [fetchSources]);

  return (
    <section
      className="panel audio-source-selector"
      aria-label="Audio source selection"
    >
      <div className="audio-source-selector__header">
        <h3 className="panel-title">Audio Sources</h3>
        <button
          className="audio-source-selector__refresh"
          onClick={handleRefresh}
          disabled={isCapturing}
          aria-label="Refresh audio sources"
          title="Refresh sources"
        >
          🔄
        </button>
      </div>

      {audioSources.length === 0 ? (
        <div className="audio-source-selector__empty">
          <p className="panel-empty">No audio sources found</p>
          <button
            className="audio-source-selector__retry"
            onClick={handleRefresh}
          >
            Retry
          </button>
        </div>
      ) : (
        <div className="audio-source-selector__groups">
          {grouped.map(([groupName, { icon, sources }]) => (
            <div key={groupName} className="audio-source-selector__group">
              <h4 className="audio-source-selector__group-label">
                <span aria-hidden="true">{icon}</span> {groupName}
              </h4>
              <ul
                className="source-list"
                role="group"
                aria-label={`${groupName} sources`}
              >
                {sources.map((source) => {
                  const isSelected = selectedSourceIds.includes(source.id);
                  const isSystemDefault =
                    source.source_type.type === "SystemDefault";
                  return (
                    <li
                      key={source.id}
                      className={`source-item ${isSelected ? "source-item--selected" : ""} ${isCapturing ? "source-item--disabled" : ""}`}
                      onClick={() => handleSelect(source.id)}
                      role="checkbox"
                      aria-checked={isSelected}
                      tabIndex={0}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          handleSelect(source.id);
                        }
                      }}
                    >
                      <span
                        className={`source-item__checkbox ${isSelected ? "source-item__checkbox--checked" : ""}`}
                        aria-hidden="true"
                      />
                      <span className="source-item__name">{source.name}</span>
                      {isSystemDefault && (
                        <span className="source-item__badge">Default</span>
                      )}
                      {isSelected && (
                        <span className="source-item__check" aria-hidden="true">
                          ✓
                        </span>
                      )}
                    </li>
                  );
                })}
              </ul>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

export default AudioSourceSelector;
