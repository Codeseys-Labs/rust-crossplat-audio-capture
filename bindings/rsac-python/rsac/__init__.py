"""rsac — Rust Cross-Platform Audio Capture (Python bindings).

A streaming-first audio capture library. Captures system audio,
per-application audio, or process-tree audio on Windows (WASAPI),
Linux (PipeWire), and macOS (CoreAudio Process Tap).

Quick start::

    import rsac

    # Query platform capabilities
    caps = rsac.platform_capabilities()
    print(f"Backend: {caps.backend_name}")
    print(f"App capture: {caps.supports_application_capture}")

    # List audio devices
    for dev in rsac.list_devices():
        print(f"  {dev.name} (default={dev.is_default})")

    # Capture audio (streaming-first)
    with rsac.AudioCapture(target=rsac.CaptureTarget.system_default()) as cap:
        for buffer in cap:
            print(f"Got {buffer.num_frames} frames, RMS={buffer.rms():.4f}")
"""

from rsac._rsac import (
    # Core classes
    AudioCapture,
    AudioBuffer,
    AudioDevice,
    CaptureTarget,
    PlatformCapabilities,
    # Module-level functions
    list_devices,
    platform_capabilities,
    # Exceptions
    RsacError,
    DeviceNotFoundError,
    DeviceNotAvailableError,
    PlatformNotSupportedError,
    StreamError,
    ConfigurationError,
    PermissionDeniedError,
    ApplicationNotFoundError,
    CaptureTimeoutError,
    BackendError,
    # Metadata
    __version__,
)

__all__ = [
    # Core classes
    "AudioCapture",
    "AudioBuffer",
    "AudioDevice",
    "CaptureTarget",
    "PlatformCapabilities",
    # Module-level functions
    "list_devices",
    "platform_capabilities",
    # Exceptions
    "RsacError",
    "DeviceNotFoundError",
    "DeviceNotAvailableError",
    "PlatformNotSupportedError",
    "StreamError",
    "ConfigurationError",
    "PermissionDeniedError",
    "ApplicationNotFoundError",
    "CaptureTimeoutError",
    "BackendError",
    # Metadata
    "__version__",
]
