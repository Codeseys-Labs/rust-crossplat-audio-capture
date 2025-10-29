# Testing Strategy

This document explains the testing approach for the cross-platform audio capture library.

## Test Layers

### 1. **Unit Tests** (No Hardware Required) ✅

**Location:** `tests/unit_tests.rs`

**Purpose:** Validate API design, data structures, and configuration without requiring actual audio hardware.

**What we test:**
- Configuration builders and validation
- Audio buffer creation and metadata
- Sample format specifications
- Device selectors and naming
- Error handling paths
- Data structure cloning and serialization

**Run with:**
```bash
cargo test --lib
cargo test --test unit_tests
```

**CI Status:** ✅ Runs on all platforms (Linux, Windows, macOS)

---

### 2. **Integration Tests** (Require Hardware) ⚠️

**Location:**
- `src/bin/dynamic_vlc_capture.rs`
- `scripts/test-vlc-capture.sh` (Linux)
- `scripts/test-vlc-capture-windows.ps1` (Windows)

**Purpose:** Test actual audio capture with real applications (VLC).

**What we test:**
- Process discovery (finding VLC)
- Audio stream capture from specific applications
- PipeWire/WASAPI/CoreAudio backends
- WAV file creation
- Audio signal validation (not just silence)

**Challenges:**
- Requires audio devices/virtual audio routing
- Needs VLC installed and playing audio
- Complex CI setup (PipeWire daemons, Virtual Audio Driver, etc.)
- Timing-sensitive
- Platform-specific quirks

**Run with:**
```bash
# Linux
./scripts/test-vlc-capture.sh

# Windows
./scripts/test-vlc-capture-windows.ps1
```

**CI Status:** ⚠️ Runs on Linux/Windows but requires careful setup

---

### 3. **Compilation Tests** (Verify Builds) ✅

**Location:** GitHub Actions workflows

**Purpose:** Ensure code compiles on all platforms even without audio hardware.

**What we test:**
- Platform-specific feature compilation
- Dependencies resolve correctly
- Examples build successfully
- No missing symbols or linking errors

**Run with:**
```bash
# macOS
cargo build --no-default-features --features feat_macos

# Linux
cargo build --no-default-features --features feat_linux

# Windows
cargo build --no-default-features --features feat_windows
```

**CI Status:** ✅ Works reliably

---

## Validation Improvements (Recent Changes)

### Problem
Tests were passing even when audio capture failed because:
- Empty WAV files were created on failure
- Scripts only checked exit codes, not audio content
- Silent captures (all zeros) were considered success

### Solution
1. **Silence Detection** - Fail if captured audio is all zeros
2. **File Size Validation** - Fail if WAV < 1KB
3. **Signal Validation** - Check for actual audio amplitude
4. **Proper Error Codes** - Return errors instead of creating empty files

**Implementation:** See commit 9ab963a

---

## CI/CD Strategy

### Fast Tests (Every Commit)
- ✅ Linting and formatting
- ✅ Unit tests (no hardware)
- ✅ Compilation tests (all platforms)
- ⏱️ **Duration:** ~5 minutes

### Slow Tests (Manual/Nightly)
- ⚠️ Integration tests with VLC
- ⚠️ Hardware-dependent captures
- ⚠️ Virtual audio device setup
- ⏱️ **Duration:** ~15-20 minutes

---

## Testing Locally

### Without Audio Hardware

```bash
# Run all unit tests
cargo test --lib --all-features

# Run specific test
cargo test --lib test_audio_buffer_creation

# Check compilation for all platforms
cargo build --lib --no-default-features --features feat_linux
cargo build --lib --no-default-features --features feat_windows
cargo build --lib --no-default-features --features feat_macos
```

### With Audio Hardware

```bash
# Linux (PipeWire)
cargo run --bin dynamic_vlc_capture --features feat_linux 10

# Windows (WASAPI)
cargo run --bin dynamic_vlc_capture --no-default-features --features feat_windows 10

# macOS (CoreAudio)
cargo run --example macos_application_capture --no-default-features --features feat_macos
```

---

## Platform-Specific Notes

### Linux (PipeWire)
- **Requirements:** PipeWire running, audio group membership
- **Test Audio Source:** VLC playing from URL or local file
- **Validation:** Monitor stream creation, node discovery
- **Common Issues:** Permissions, PipeWire not running, no audio sources

### Windows (WASAPI)
- **Requirements:** Virtual Audio Driver, audio services running
- **Test Audio Source:** VLC with WaveOut → Virtual Audio Speaker
- **Validation:** Process Loopback creation, audio session enumeration
- **Common Issues:** Driver installation, certificate trust, device routing

### macOS (CoreAudio)
- **Requirements:** macOS 14.4+ for Process Tap, Screen Recording permission
- **Test Audio Source:** System audio or specific app via Process Tap
- **Validation:** Device enumeration, HAL output unit
- **Common Issues:** Permissions, macOS version, no virtual audio device in CI

---

## Future Improvements

### Short-term
- [ ] Add mock audio sources for testing without VLC
- [ ] Reduce integration test dependencies
- [ ] Add property-based testing for buffer handling
- [ ] Test error recovery paths

### Long-term
- [ ] Docker-based integration tests
- [ ] Performance benchmarks
- [ ] Fuzz testing for audio processing
- [ ] Cross-platform test parity

---

## Test Coverage

Current coverage (estimate):
- **API/Config:** ~80% (unit tests)
- **Platform Backends:** ~60% (integration tests)
- **Error Handling:** ~40% (needs improvement)
- **Edge Cases:** ~30% (needs improvement)

**Goal:** 70%+ coverage with focus on critical paths

---

## Contributing

When adding new features:

1. **Always add unit tests** - Even if hardware-dependent, test the API/config
2. **Update integration tests** - If changing capture logic
3. **Test on actual hardware** - Before submitting PR
4. **Update this document** - If changing testing strategy

See also: `CONTRIBUTING.md` (if exists)
