import { useEffect, useState } from "react";
import { useAudioGraphStore } from "../store";
import type {
  AsrProvider,
  LlmApiConfig,
  LlmProvider,
  ModelReadiness,
} from "../types";

/** Format bytes to a human-readable size string (e.g. "466 MB"). */
function formatSize(bytes: number | null): string {
  if (bytes === null || bytes === 0) return "—";
  const mb = bytes / (1024 * 1024);
  if (mb >= 1024) {
    return `${(mb / 1024).toFixed(1)} GB`;
  }
  return `${Math.round(mb)} MB`;
}

/** Map a ModelReadiness value to a CSS modifier and label. */
function readinessBadge(status: ModelReadiness): {
  cls: string;
  label: string;
} {
  switch (status) {
    case "Ready":
      return { cls: "status-badge--ready", label: "Ready" };
    case "NotDownloaded":
      return { cls: "status-badge--not-downloaded", label: "Not Downloaded" };
    case "Invalid":
      return { cls: "status-badge--invalid", label: "Invalid" };
  }
}

function SettingsPage() {
  const {
    settings,
    models,
    modelStatus,
    settingsLoading,
    isDownloading,
    downloadProgress,
    isDeletingModel,
    closeSettings,
    saveSettings,
    downloadModel,
    deleteModel,
  } = useAudioGraphStore();

  // ── Local form state ──────────────────────────────────────────────────
  const [asrType, setAsrType] = useState<"local_whisper" | "api">(
    "local_whisper",
  );
  const [asrEndpoint, setAsrEndpoint] = useState("");
  const [asrApiKey, setAsrApiKey] = useState("");
  const [asrModel, setAsrModel] = useState("");

  const [llmType, setLlmType] = useState<"local_llama" | "api">("api");
  const [llmEndpoint, setLlmEndpoint] = useState("http://localhost:11434/v1");
  const [llmApiKey, setLlmApiKey] = useState("");
  const [llmModel, setLlmModel] = useState("llama3.2");
  const [llmMaxTokens, setLlmMaxTokens] = useState(2048);
  const [llmTemperature, setLlmTemperature] = useState(0.7);

  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  // Sync local state when settings are loaded
  useEffect(() => {
    if (!settings) return;

    // ASR provider
    if (settings.asr_provider.type === "api") {
      setAsrType("api");
      setAsrEndpoint(settings.asr_provider.endpoint);
      setAsrApiKey(settings.asr_provider.api_key);
      setAsrModel(settings.asr_provider.model);
    } else {
      setAsrType("local_whisper");
    }

    // LLM provider
    if (settings.llm_provider.type === "api") {
      setLlmType("api");
      setLlmEndpoint(settings.llm_provider.endpoint);
      setLlmApiKey(settings.llm_provider.api_key);
      setLlmModel(settings.llm_provider.model);
    } else {
      setLlmType("local_llama");
    }

    // LLM config (advanced — max_tokens / temperature)
    if (settings.llm_api_config) {
      setLlmMaxTokens(settings.llm_api_config.max_tokens);
      setLlmTemperature(settings.llm_api_config.temperature);
    }
  }, [settings]);

  // ── Handlers ──────────────────────────────────────────────────────────
  const handleSave = async () => {
    const asrProvider: AsrProvider =
      asrType === "api"
        ? {
            type: "api",
            endpoint: asrEndpoint,
            api_key: asrApiKey,
            model: asrModel,
          }
        : { type: "local_whisper" };

    const llmProvider: LlmProvider =
      llmType === "api"
        ? {
            type: "api",
            endpoint: llmEndpoint,
            api_key: llmApiKey,
            model: llmModel,
          }
        : { type: "local_llama" };

    const llmConfig: LlmApiConfig | null =
      llmType === "api" && llmEndpoint
        ? {
            endpoint: llmEndpoint,
            api_key: llmApiKey || null,
            model: llmModel,
            max_tokens: llmMaxTokens,
            temperature: llmTemperature,
          }
        : null;

    await saveSettings({
      asr_provider: asrProvider,
      llm_provider: llmProvider,
      llm_api_config: llmConfig,
      audio_settings: settings?.audio_settings ?? {
        sample_rate: 16000,
        channels: 1,
      },
    });
  };

  const handleDeleteClick = (filename: string) => {
    if (confirmDelete === filename) {
      deleteModel(filename);
      setConfirmDelete(null);
    } else {
      setConfirmDelete(filename);
    }
  };

  // ── Render ────────────────────────────────────────────────────────────
  return (
    <div className="settings-overlay" onClick={closeSettings}>
      <div className="settings-modal" onClick={(e) => e.stopPropagation()}>
        {/* Header */}
        <div className="settings-header">
          <h2 className="settings-header__title">Settings</h2>
          <button
            className="settings-header__close"
            onClick={closeSettings}
            aria-label="Close settings"
          >
            ✕
          </button>
        </div>

        {settingsLoading ? (
          <div className="settings-content settings-content--loading">
            <p>Loading settings…</p>
          </div>
        ) : (
          <div className="settings-content">
            {/* ── Models Section ─────────────────────────────────── */}
            <div className="settings-section">
              <h3 className="settings-section__title">Models</h3>
              {models.map((model) => {
                const status =
                  modelStatus && model.name.toLowerCase().includes("whisper")
                    ? modelStatus.whisper
                    : modelStatus && model.name.toLowerCase().includes("vad")
                      ? modelStatus.vad
                      : modelStatus
                        ? modelStatus.llm
                        : ("NotDownloaded" as ModelReadiness);

                const badge = readinessBadge(status);
                const isThisDownloading =
                  isDownloading && downloadProgress?.model_name === model.name;
                const isThisDeleting = isDeletingModel === model.filename;

                return (
                  <div className="model-card" key={model.filename}>
                    <div className="model-card__header">
                      <div>
                        <span className="model-card__name">{model.name}</span>
                        <span className={`status-badge ${badge.cls}`}>
                          {badge.label}
                        </span>
                      </div>
                      <span className="model-card__size">
                        {formatSize(model.size_bytes)}
                      </span>
                    </div>
                    {model.description && (
                      <p className="model-card__description">
                        {model.description}
                      </p>
                    )}

                    <div className="model-card__actions">
                      {!model.is_downloaded && (
                        <button
                          className="settings-btn settings-btn--primary"
                          onClick={() => downloadModel(model.filename)}
                          disabled={isDownloading}
                        >
                          {isThisDownloading ? "Downloading…" : "Download"}
                        </button>
                      )}
                      {model.is_downloaded && (
                        <button
                          className="settings-btn settings-btn--danger"
                          onClick={() => handleDeleteClick(model.filename)}
                          disabled={isThisDeleting}
                        >
                          {isThisDeleting
                            ? "Deleting…"
                            : confirmDelete === model.filename
                              ? "Confirm Delete"
                              : "Delete"}
                        </button>
                      )}
                    </div>

                    {/* Download progress bar */}
                    {isThisDownloading && downloadProgress && (
                      <div className="download-progress">
                        <div
                          className="download-progress__bar"
                          style={{ width: `${downloadProgress.percent}%` }}
                        />
                      </div>
                    )}
                  </div>
                );
              })}
              {models.length === 0 && (
                <p className="settings-section__empty">No models available.</p>
              )}
            </div>

            {/* ── ASR Provider Section ───────────────────────────── */}
            <div className="settings-section">
              <h3 className="settings-section__title">ASR Provider</h3>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "local_whisper"}
                    onChange={() => setAsrType("local_whisper")}
                  />
                  <span>Local Whisper</span>
                  {asrType === "local_whisper" && modelStatus && (
                    <span
                      className={`status-badge ${readinessBadge(modelStatus.whisper).cls}`}
                    >
                      {readinessBadge(modelStatus.whisper).label}
                    </span>
                  )}
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="asr-provider"
                    checked={asrType === "api"}
                    onChange={() => setAsrType("api")}
                  />
                  <span>OpenAI-compatible API</span>
                </label>
              </div>

              {asrType === "api" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Endpoint URL
                    </label>
                    <input
                      className="settings-input"
                      type="text"
                      value={asrEndpoint}
                      onChange={(e) => setAsrEndpoint(e.target.value)}
                      placeholder="https://api.openai.com/v1"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">API Key</label>
                    <input
                      className="settings-input"
                      type="password"
                      value={asrApiKey}
                      onChange={(e) => setAsrApiKey(e.target.value)}
                      placeholder="sk-..."
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">Model</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={asrModel}
                      onChange={(e) => setAsrModel(e.target.value)}
                      placeholder="whisper-1"
                    />
                  </div>
                </div>
              )}
            </div>

            {/* ── LLM Provider Section ───────────────────────────── */}
            <div className="settings-section">
              <h3 className="settings-section__title">LLM Provider</h3>
              <div className="settings-radio-group">
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="llm-provider"
                    checked={llmType === "local_llama"}
                    onChange={() => setLlmType("local_llama")}
                  />
                  <span>Local LLM (LFM2-350M)</span>
                  {llmType === "local_llama" && modelStatus && (
                    <span
                      className={`status-badge ${readinessBadge(modelStatus.llm).cls}`}
                    >
                      {readinessBadge(modelStatus.llm).label}
                    </span>
                  )}
                </label>
                <label className="settings-radio">
                  <input
                    type="radio"
                    name="llm-provider"
                    checked={llmType === "api"}
                    onChange={() => setLlmType("api")}
                  />
                  <span>OpenAI-compatible API</span>
                </label>
              </div>

              {llmType === "api" && (
                <div className="settings-section__api-fields">
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Endpoint URL
                    </label>
                    <input
                      className="settings-input"
                      type="text"
                      value={llmEndpoint}
                      onChange={(e) => setLlmEndpoint(e.target.value)}
                      placeholder="https://openrouter.ai/api/v1"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">API Key</label>
                    <input
                      className="settings-input"
                      type="password"
                      value={llmApiKey}
                      onChange={(e) => setLlmApiKey(e.target.value)}
                      placeholder="sk-..."
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">Model</label>
                    <input
                      className="settings-input"
                      type="text"
                      value={llmModel}
                      onChange={(e) => setLlmModel(e.target.value)}
                      placeholder="gpt-4o-mini"
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Max Tokens ({llmMaxTokens})
                    </label>
                    <input
                      className="settings-input"
                      type="number"
                      value={llmMaxTokens}
                      onChange={(e) => setLlmMaxTokens(Number(e.target.value))}
                      min={1}
                      max={32768}
                    />
                  </div>
                  <div className="settings-field">
                    <label className="settings-field__label">
                      Temperature ({llmTemperature})
                    </label>
                    <input
                      className="settings-input"
                      type="number"
                      step="0.1"
                      value={llmTemperature}
                      onChange={(e) =>
                        setLlmTemperature(Number(e.target.value))
                      }
                      min={0}
                      max={2}
                    />
                  </div>
                </div>
              )}
            </div>
          </div>
        )}

        {/* Footer */}
        <div className="settings-footer">
          <button
            className="settings-btn settings-btn--primary"
            onClick={handleSave}
            disabled={settingsLoading}
          >
            Save Settings
          </button>
        </div>
      </div>
    </div>
  );
}

export default SettingsPage;
