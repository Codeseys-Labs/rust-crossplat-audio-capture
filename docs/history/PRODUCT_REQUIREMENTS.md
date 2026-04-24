# Product Requirements Document: rust-crossplat-audio-capture

## 1. Introduction

This document outlines the product requirements for the `rust-crossplat-audio-capture` library. The project aims to provide a comprehensive, robust, and easy-to-use Rust solution for cross-platform audio capture, supporting both system-level and application-level audio sources. This PRD details the functional and non-functional requirements necessary to achieve this vision, guiding development efforts and ensuring the library meets the needs of its users.

The library currently offers varying levels of support across Windows (WASAPI), Linux (PipeWire/PulseAudio), and macOS (CoreAudio - system only). This document defines the path forward to enhance existing capabilities and introduce new, critical features, particularly application-level capture on macOS and robust real-time audio processing support.

## 2. Goals

The primary goals for `rust-crossplat-audio-capture` are:

- **Establish Market Leadership:** Become the de-facto standard Rust library for cross-platform audio capture.
- **Comprehensive Platform Support:** Provide reliable system-level and application-level audio capture on Windows, Linux, and macOS.
- **Enable Real-Time Processing:** Offer low-latency, efficient access to audio data streams, empowering developers to build real-time audio analysis and processing applications (e.g., speaker diarization, live effects).
- **Ease of Use:** Provide a developer-friendly API that simplifies complex audio capture tasks.
- **Flexibility and Configurability:** Allow users to easily configure audio parameters (sample rate, bit depth, channels) and output formats.
- **High Performance:** Ensure efficient audio data handling with minimal overhead.
- **Reliability:** Deliver a stable and robust library that developers can depend on for production applications.

## 3. Target Audience

This library is intended for:

- **Rust Developers:** Programmers building applications in Rust that require audio capture capabilities.
- **Audio Application Developers:** Creators of software such as:
  - Audio recording and editing tools.
  - Real-time audio analysis and visualization software (e.g., spectrum analyzers, oscilloscopes).
  - Voice communication applications (e.g., VoIP clients, in-game chat).
  - Accessibility tools (e.g., live transcription, sound recognition).
  - Machine learning applications involving audio (e.g., speaker diarization, keyword spotting, acoustic event detection).
- **Researchers:** Individuals working on audio processing, speech recognition, and related fields who need a reliable tool for audio data acquisition in Rust.

## 4. Functional Requirements

### 4.1. Platform-Specific Audio Capture

#### 4.1.1. Windows

- **System-Level Capture:**
  - Maintain and confirm robust support using WASAPI (Windows Audio Session API) loopback.
- **Application-Level Capture:**
  - Maintain and confirm robust support using WASAPI to capture audio from specific applications/processes.
- **Backend Strategy:** WASAPI is the primary and sole backend for Windows.

#### 4.1.2. Linux

- **System-Level Capture:**
  - Prioritize PipeWire as the primary backend.
  - Provide PulseAudio as a fallback mechanism where PipeWire is unavailable.
  - Investigate JACK Audio Connection Kit for scenarios requiring ultra-low-latency if PipeWire proves insufficient for specific real-time processing needs.
- **Application-Level Capture:**
  - Prioritize PipeWire for capturing audio from specific applications.
  - Provide PulseAudio as a fallback for application-level capture.
- **Backend Strategy:** PipeWire (primary), PulseAudio (fallback). JACK (investigation for specialized low-latency).

#### 4.1.3. macOS

- **System-Level Capture:**
  - Maintain robust support using CoreAudio (e.g., capturing default output device).
- **Application-Level Capture:**
  - **High Priority:** Research and implement reliable application-level audio capture. This is a key development focus.
  - Potential approaches to investigate include:
    - Utilizing public CoreAudio APIs if undocumented or less common features allow.
    - Exploring the creation of a virtual audio device.
    - Investigating solutions similar to existing third-party tools (e.g., Soundflower, BlackHole, Loopback) and how they achieve this, potentially requiring privileged helper tools or kernel extensions (with careful consideration of security and usability implications).
- **Backend Strategy:** CoreAudio for system-level. Dedicated research and implementation for application-level capture.

### 4.2. Audio Backend Management

- The library must allow for clear selection or automatic detection of available audio backends on each platform.
- Provide clear error handling and reporting for backend initialization failures.

### 4.3. Audio Recording Capabilities

- Enable straightforward recording of captured audio streams.
- Support for common audio formats (e.g., WAV, potentially MP3/AAC via integration with encoding libraries).
- Allow configuration of output file paths and naming conventions.

### 4.4. Real-Time Audio Processing Enablement

- **Low-Latency Stream Access:** Provide an API that allows developers to access audio data buffers with minimal delay from the moment of capture.
  - This includes efficient buffer management and timely delivery of audio frames.
- **Callback Mechanism/Async Stream:** Offer a flexible way for client code to process audio data as it arrives (e.g., via callbacks, asynchronous streams/iterators).
- **Data Format:** Deliver audio data in a raw, uncompressed format (e.g., PCM float or PCM S16LE) suitable for direct processing.
- **API Design:** The API should be designed to minimize copying and allocations in the hot path of audio data delivery to support performance-critical applications.
- **Timestamping:** Provide accurate timestamps for audio buffers to aid in synchronization and analysis.

### 4.5. Configurable Parameters

The library must allow users to configure:

- **Sample Rate:** (e.g., 44.1 kHz, 48 kHz, 96 kHz). The library should attempt to capture at the native rate of the source if not specified, or resample if a specific rate is requested and direct capture at that rate is not possible (resampling capabilities might be a separate feature or require integration with other crates).
- **Bit Depth:** (e.g., 16-bit, 24-bit, 32-bit float).
- **Number of Channels:** (e.g., Mono, Stereo, multi-channel if supported by the backend and source).
- **Input Device Selection:** Where applicable (e.g., choosing a specific microphone or loopback device if multiple are available).
- **Application Selection (for app-level capture):** Provide a mechanism to specify the target application (e.g., by process ID, window name, or other identifiable means).

## 5. Non-Functional Requirements

### 5.1. Performance

- **Low Latency:** Minimize the delay between audio generation by the source and its availability to the library user, especially for real-time processing use cases. Target latencies should be competitive with native platform capabilities.
- **CPU Efficiency:** Minimize CPU usage during capture and data delivery.
- **Memory Efficiency:** Minimize memory footprint and avoid unnecessary allocations, especially in audio processing callbacks or stream handling.

### 5.2. Reliability & Stability

- **Robustness:** The library should handle errors gracefully (e.g., device disconnections, backend failures) and provide clear diagnostic information.
- **Stability:** Avoid crashes, memory leaks, or other issues that could impact the stability of applications using the library.
- **Thread Safety:** Ensure that the API is thread-safe where appropriate, allowing for use in multi-threaded applications.

### 5.3. Usability & API Design

- **Simplicity:** Offer a clean, intuitive, and well-documented API that is easy to learn and use, even for developers not deeply familiar with audio programming.
- **Rust Idioms:** Adhere to Rust best practices and idiomatic design patterns (e.g., use of `Result` for error handling, `Option` for optional values, iterators/streams).
- **Discoverability:** Make it easy for developers to find the features and configurations they need.
- **Examples:** Provide comprehensive examples demonstrating various use cases, including basic recording, application-specific capture, and real-time stream processing.
- **Error Handling:** Errors should be descriptive and actionable.

### 5.4. Maintainability

- **Modular Design:** Code should be well-structured and modular to facilitate maintenance and future enhancements.
- **Comprehensive Testing and CI/CD Strategy:** To ensure code quality, maintain reliability across platforms, and catch regressions early in the development cycle, the following practices and infrastructure are required:
  - **Comprehensive Docker-based Testing:**
    - Establish and maintain Docker environments for each supported platform (Windows, Linux, macOS) to run automated tests.
    - Ensure tests cover various configurations, including different audio backends (WASAPI, PipeWire, PulseAudio, CoreAudio), and, where feasible within a Dockerized environment, different audio parameters (e.g., sample rates, formats).
    - The existing Dockerfiles in the [`docker/`](docker/) directory should be leveraged, maintained, and expanded as necessary to support this testing strategy.
  - **Continuous Integration/Continuous Deployment (CI/CD):**
    - Implement and maintain a robust CI/CD pipeline (e.g., using GitHub Actions or a similar service).
    - The pipeline must automatically build the library and execute the full suite of Dockerized tests on every push to the main development branch and for every pull request submitted.
    - The pipeline should provide clear feedback on build status and test results.
  - **Test Coverage:**
    - Aim for and maintain a high level of test coverage for the library's core functionalities, platform-specific implementations, and critical audio processing paths.
    - Coverage metrics should be tracked, and efforts made to improve coverage for new and existing code.

### 5.5. Compatibility

- Maintain compatibility with a reasonable range of Rust compiler versions.
- Clearly document any platform-specific dependencies or build requirements.

## 6. Future Considerations / Out of Scope (Optional)

### 6.1. Future Considerations

- **Advanced Resampling:** Built-in high-quality audio resampling capabilities.
- **Effect Chaining:** APIs to support simple audio effect application within the library.
- **WebAssembly (Wasm) Support:** Investigate potential for capturing audio in Wasm environments (e.g., browser extensions, desktop apps using Wasm).
- **Network Streaming:** Direct support for streaming captured audio over a network.
- **VAD (Voice Activity Detection):** Built-in or example integration for voice activity detection.
- **Support for more audio backends:** e.g. ALSA on Linux if specific needs arise not covered by PipeWire/PulseAudio.

### 6.2. Out of Scope (for this iteration)

- **Audio Encoding/Decoding:** The library will focus on raw audio capture. Encoding (e.g., to MP3, AAC, Opus) and decoding will be left to specialized external crates, though examples of integration may be provided.
- **Audio Playback:** This library is solely focused on audio capture.
- **Complex Digital Audio Workstation (DAW) Features:** Advanced mixing, routing, and plugin hosting are beyond the scope.
- **Kernel-Level Development (unless absolutely necessary for macOS app capture):** Efforts will be focused on user-space solutions where possible. Any kernel-level components would require significant justification and careful consideration.
