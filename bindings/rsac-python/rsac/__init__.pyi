"""Type stubs for rsac — Rust Cross-Platform Audio Capture."""

from typing import Awaitable, Iterator, Optional

__version__: str

# ── Exceptions ────────────────────────────────────────────────────────────

class RsacError(OSError):
    """Base exception for all rsac errors."""
    ...

class DeviceNotFoundError(RsacError):
    """The requested audio device was not found."""
    ...

class DeviceNotAvailableError(RsacError):
    """The audio device exists but is not currently available."""
    ...

class PlatformNotSupportedError(RsacError):
    """The requested feature is not supported on this platform."""
    ...

class StreamError(RsacError):
    """An error occurred during audio stream operation."""
    ...

class ConfigurationError(ValueError):
    """Invalid capture configuration."""
    ...

class PermissionDeniedError(RsacError):
    """Permission denied for the requested audio operation."""
    ...

class ApplicationNotFoundError(RsacError):
    """The target application for capture was not found."""
    ...

class CaptureTimeoutError(RsacError):
    """An audio capture operation timed out."""
    ...

class BackendError(RsacError):
    """A platform-specific audio backend error occurred."""
    ...

# ── CaptureTarget ─────────────────────────────────────────────────────────

class CaptureTarget:
    """Specifies what audio to capture.

    Use the static constructor methods to create a target.
    """

    @staticmethod
    def system_default() -> CaptureTarget:
        """Capture from the system default audio device / mix."""
        ...

    @staticmethod
    def device(device_id: str) -> CaptureTarget:
        """Capture from a specific device by its platform ID string."""
        ...

    @staticmethod
    def application(app_id: str) -> CaptureTarget:
        """Capture audio from an application by its session/application ID."""
        ...

    @staticmethod
    def application_by_name(name: str) -> CaptureTarget:
        """Capture audio from the first application whose name matches."""
        ...

    @staticmethod
    def process_tree(pid: int) -> CaptureTarget:
        """Capture audio from a process and its child processes by PID."""
        ...

    @staticmethod
    def parse(spec: str) -> CaptureTarget:
        """Parse a capture target from its canonical string grammar.

        Grammar (case-insensitive scheme): ``"system"``, ``"device:<id>"``,
        ``"app:<id>"``, ``"name:<n>"``, ``"tree:<pid>"``.

        Raises:
            ConfigurationError: If ``spec`` is not a valid target string.
        """
        ...

# ── AudioBuffer ───────────────────────────────────────────────────────────

class AudioBuffer:
    """A buffer of interleaved audio samples (f32).

    Returned by AudioCapture.read() and the iterator protocol.
    """

    @property
    def num_frames(self) -> int:
        """Number of audio frames (samples per channel)."""
        ...

    @property
    def channels(self) -> int:
        """Number of audio channels."""
        ...

    @property
    def sample_rate(self) -> int:
        """Sample rate in Hz."""
        ...

    @property
    def sample_count(self) -> int:
        """Total number of interleaved samples."""
        ...

    @property
    def duration_secs(self) -> float:
        """Duration of the audio in this buffer, in seconds."""
        ...

    @property
    def is_empty(self) -> bool:
        """Whether the buffer contains no samples."""
        ...

    def to_list(self) -> list[float]:
        """Return the interleaved sample data as a Python list of floats."""
        ...

    def to_bytes(self) -> bytes:
        """Return raw sample data as bytes (little-endian f32, 4 bytes/sample)."""
        ...

    def channel_data(self, channel: int) -> Optional[list[float]]:
        """Extract samples for a single channel (0-indexed).

        Returns None if the channel index is out of range.
        """
        ...

    def rms(self) -> float:
        """Compute the RMS (root mean square) level of all samples."""
        ...

    def peak(self) -> float:
        """Return the peak absolute sample value across all channels."""
        ...

    def rms_dbfs(self) -> float:
        """RMS level in dBFS. Returns ``-inf`` for digital silence."""
        ...

    def peak_dbfs(self) -> float:
        """Peak level in dBFS. Returns ``-inf`` for digital silence."""
        ...

    def channel_rms(self, channel: int) -> Optional[float]:
        """RMS level of a single channel (0-indexed), or None if out of range."""
        ...

    def channel_peak(self, channel: int) -> Optional[float]:
        """Peak sample value of a single channel (0-indexed), or None if out of range."""
        ...

    def __len__(self) -> int: ...
    def __bool__(self) -> bool: ...
    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

# ── AudioDevice ───────────────────────────────────────────────────────────

class AudioDevice:
    """Information about an audio device on the system."""

    @property
    def id(self) -> str:
        """Platform-specific device identifier string."""
        ...

    @property
    def name(self) -> str:
        """Human-readable device name."""
        ...

    @property
    def is_default(self) -> bool:
        """Whether this is the system default device."""
        ...

    def __repr__(self) -> str: ...
    def __str__(self) -> str: ...

# ── PlatformCapabilities ──────────────────────────────────────────────────

class PlatformCapabilities:
    """Reports what the current platform's audio backend supports."""

    @property
    def supports_system_capture(self) -> bool: ...

    @property
    def supports_application_capture(self) -> bool: ...

    @property
    def supports_process_tree_capture(self) -> bool: ...

    @property
    def supports_device_selection(self) -> bool: ...

    @property
    def supports_device_change_notifications(self) -> bool: ...

    @property
    def requires_user_consent(self) -> bool: ...

    @property
    def max_channels(self) -> int: ...

    @property
    def sample_rate_range(self) -> tuple[int, int]: ...

    @property
    def supported_sample_formats(self) -> list[str]: ...

    @property
    def supported_sample_rates(self) -> list[int]: ...

    @property
    def backend_name(self) -> str: ...

    def __repr__(self) -> str: ...

# ── StreamStats ───────────────────────────────────────────────────────────

class StreamStats:
    """A point-in-time snapshot of stream statistics.

    Returned by :meth:`AudioCapture.stream_stats`. Read-only.
    """

    @property
    def overruns(self) -> int:
        """Buffers dropped due to ring-buffer overflow."""
        ...

    @property
    def buffers_captured(self) -> int:
        """Buffers delivered to the consumer since the stream started."""
        ...

    @property
    def buffers_dropped(self) -> int:
        """Buffers dropped due to overflow (alias of ``overruns``)."""
        ...

    @property
    def buffers_pushed(self) -> int:
        """Buffers enqueued by the OS audio callback since the stream started."""
        ...

    @property
    def uptime_secs(self) -> float:
        """Seconds the stream has been running (0.0 when stopped)."""
        ...

    @property
    def is_running(self) -> bool:
        """Whether the stream is currently capturing."""
        ...

    @property
    def format_description(self) -> str:
        """Human-readable description of the audio format being captured."""
        ...

    def dropped_ratio(self) -> float:
        """Fraction of buffers lost to overflow, in 0.0..=1.0 (0.0 when none)."""
        ...

    def __repr__(self) -> str: ...

# ── BackpressureReport ──────────────────────────────────────────────────────

class BackpressureReport:
    """A windowed snapshot of producer backpressure.

    Returned by :meth:`AudioCapture.backpressure_report`. Read-only.

    Unlike the all-or-nothing :attr:`is_under_backpressure` flag, ``drop_rate``
    surfaces sustained partial loss (e.g. a steady 1-in-3 drop pattern).
    """

    @property
    def window_secs(self) -> float:
        """Wall-clock span the tallies cover, in seconds (0.0 when unattributed)."""
        ...

    @property
    def pushed(self) -> int:
        """Buffers successfully pushed by the producer within the window."""
        ...

    @property
    def dropped(self) -> int:
        """Buffers dropped due to ring-buffer overflow within the window."""
        ...

    @property
    def drop_rate(self) -> float:
        """Fraction of buffers lost within the window, in 0.0..=1.0 (0.0 when none)."""
        ...

    @property
    def is_under_backpressure(self) -> bool:
        """The legacy consecutive-drop backpressure flag."""
        ...

    def __repr__(self) -> str: ...

# ── AudioFormat ───────────────────────────────────────────────────────────

class AudioFormat:
    """The negotiated audio delivery format of a running capture.

    Returned by the :attr:`AudioCapture.format` getter. Read-only.
    """

    @property
    def sample_rate(self) -> int:
        """Samples per second (Hz)."""
        ...

    @property
    def channels(self) -> int:
        """Number of interleaved channels."""
        ...

    @property
    def sample_format(self) -> str:
        """Sample type as a string: "f32", "i16", "i24", or "i32"."""
        ...

    def __repr__(self) -> str: ...

# ── AudioCapture ──────────────────────────────────────────────────────────

class AudioCapture:
    """The main audio capture class.

    Supports the synchronous (__enter__/__exit__) and asynchronous
    (__aenter__/__aexit__) context-manager protocols, plus the iterator
    protocol (__iter__/__next__) for streaming audio. A finalizer
    (__del__) stops any still-running OS stream if the capture is dropped
    without an explicit close.

    Args:
        target: What to capture (default: CaptureTarget.system_default()).
        sample_rate: Sample rate in Hz (default: 48000).
        channels: Number of channels (default: 2).
        buffer_size: Optional buffer size in frames.
    """

    def __init__(
        self,
        target: Optional[CaptureTarget] = None,
        sample_rate: int = 48000,
        channels: int = 2,
        buffer_size: Optional[int] = None,
    ) -> None: ...

    def start(self) -> None:
        """Start audio capture."""
        ...

    def stop(self) -> None:
        """Stop audio capture."""
        ...

    @property
    def is_running(self) -> bool:
        """Whether the capture is currently running."""
        ...

    def try_read(self) -> Optional[AudioBuffer]:
        """Read the next audio buffer (non-blocking).

        Returns an AudioBuffer if data is available, or None.
        """
        ...

    def read(self) -> AudioBuffer:
        """Read the next audio buffer (blocking).

        Blocks until audio data is available. The GIL is released
        during the wait.

        Terminal-observable: once the stream has ended (after ``stop()``
        or a fatal backend error) this raises the stream's true terminal
        error promptly, matching iteration, instead of a recoverable
        "not running" error.
        """
        ...

    @property
    def overrun_count(self) -> int:
        """Number of audio buffers dropped due to ring buffer overflow."""
        ...

    def stream_stats(self) -> StreamStats:
        """Return a point-in-time snapshot of stream statistics.

        On a closed capture, returns a default snapshot (all counters zero,
        ``is_running == False``).
        """
        ...

    def backpressure_report(self) -> BackpressureReport:
        """Return a windowed snapshot of producer backpressure.

        On a closed capture, returns a default report (all counters zero,
        ``drop_rate == 0.0``, ``is_under_backpressure == False``).
        """
        ...

    @property
    def format(self) -> Optional[AudioFormat]:
        """The negotiated audio delivery format, or None if not running."""
        ...

    def close(self) -> None:
        """Close the capture and release all resources."""
        ...

    def __enter__(self) -> AudioCapture: ...
    def __exit__(self, exc_type: type | None, exc_val: BaseException | None, exc_tb: object | None) -> bool: ...
    def __aenter__(self) -> Awaitable[AudioCapture]: ...
    def __aexit__(self, exc_type: type | None, exc_val: BaseException | None, exc_tb: object | None) -> Awaitable[None]: ...
    def __iter__(self) -> Iterator[AudioBuffer]: ...
    def __next__(self) -> AudioBuffer: ...
    def __del__(self) -> None: ...
    def __repr__(self) -> str: ...

# ── Module-level functions ────────────────────────────────────────────────

def list_devices() -> list[AudioDevice]:
    """List all available audio devices on the system."""
    ...

def platform_capabilities() -> PlatformCapabilities:
    """Query the platform's audio capture capabilities."""
    ...
