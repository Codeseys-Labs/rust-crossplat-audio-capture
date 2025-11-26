# Docker-Based Cross-Platform Testing

This document describes the comprehensive Docker-based testing setup for the Rust Cross-Platform Audio Capture project. The setup enables local testing of cross-compilation for Linux, Windows, and macOS using industry-standard tools.

## Overview

The Docker testing system provides:

- **Linux Testing**: Native compilation with PipeWire, PulseAudio, and ALSA support
- **Windows Cross-Compilation**: Using `cargo-xwin` with MSVC toolchain and Wine for testing
- **macOS Cross-Compilation**: Using `osxcross` with macOS SDK for both x86_64 and ARM64 targets
- **Unified Test Orchestration**: Single command to test all platforms
- **Comprehensive Reporting**: HTML dashboards and JSON reports

## Quick Start

### Prerequisites

- Docker and Docker Compose installed
- Make (for using Makefile targets)
- At least 8GB of available disk space for Docker images

### Run All Platform Tests

```bash
# Run comprehensive tests for all platforms
make docker-test-all

# Quick compilation check only
make docker-test-quick

# Generate test dashboard
make docker-dashboard
```

### Run Platform-Specific Tests

```bash
# Test Linux only
make docker-test-linux

# Test Windows cross-compilation only
make docker-test-windows

# Test macOS cross-compilation only
make docker-test-macos

# Test specific platform using script
make docker-test-platform PLATFORM=linux
```

## Docker Images and Architecture

### Base Images Used

1. **Linux Testing**: `rust:1.88.0` with Ubuntu packages
2. **Windows Cross-Compilation**: Based on `cargo-xwin` approach with Wine
3. **macOS Cross-Compilation**: `joseluisq/rust-linux-darwin-builder:1.88.0` with osxcross

### Container Architecture

```
docker/
├── unified/
│   ├── Dockerfile              # Multi-stage unified container
│   └── test-runner.sh          # Unified test runner script
├── windows/
│   ├── Dockerfile.cargo-xwin   # cargo-xwin based Windows testing
│   └── test-cargo-xwin.sh      # Windows-specific test script
└── macos/
    ├── Dockerfile.cross        # osxcross based macOS testing
    └── test-cross.sh           # macOS-specific test script
```

## Available Make Targets

### Core Testing Targets

| Target | Description |
|--------|-------------|
| `docker-test-all` | Run comprehensive tests for all platforms |
| `docker-test-quick` | Quick compilation check for all platforms |
| `docker-test-unified` | Run unified cross-platform compilation tests |
| `docker-test-linux` | Run Linux audio tests with device access |
| `docker-test-windows` | Run Windows cross-compilation with cargo-xwin |
| `docker-test-macos` | Run macOS cross-compilation with osxcross |

### Development Targets

| Target | Description |
|--------|-------------|
| `docker-dev` | Start development container with all tools |
| `docker-check` | Quick compilation check for all platforms |
| `docker-cargo-xwin` | Start cargo-xwin container for Windows development |

### Utility Targets

| Target | Description |
|--------|-------------|
| `docker-build` | Build all Docker images |
| `docker-clean` | Clean Docker volumes and containers |
| `docker-logs` | Show Docker logs |
| `docker-aggregate-results` | Aggregate test results and generate reports |
| `docker-dashboard` | Generate HTML dashboard from test results |
| `docker-full-test` | Full workflow: test + aggregate + dashboard |

## Test Results and Reporting

### Result Structure

Test results are organized in the `test-results/` directory:

```
test-results/
├── linux/
│   ├── build_TIMESTAMP.log
│   ├── compilation_TIMESTAMP.json
│   └── summary_TIMESTAMP.json
├── windows/
│   ├── build_TIMESTAMP.log
│   ├── compilation_TIMESTAMP.json
│   └── summary_TIMESTAMP.json
├── macos/
│   ├── build_TIMESTAMP.log
│   ├── compilation_TIMESTAMP.json
│   └── summary_TIMESTAMP.json
└── reports/
    ├── aggregated_results_TIMESTAMP.json
    ├── dashboard_TIMESTAMP.html
    └── comprehensive_report_TIMESTAMP.html
```

### Report Types

1. **JSON Reports**: Machine-readable test results with detailed metadata
2. **HTML Dashboard**: Interactive web dashboard with visual status indicators
3. **Comprehensive Reports**: Detailed HTML reports with build logs and analysis

## Cross-Compilation Details

### Windows Cross-Compilation (cargo-xwin)

- **Target**: `x86_64-pc-windows-msvc`
- **Toolchain**: MSVC with clang-cl
- **Testing**: Wine for binary execution testing
- **Features**: WASAPI and DirectSound audio backends

```bash
# Manual cargo-xwin usage
docker run --rm -v $(pwd):/io -w /io messense/cargo-xwin \
  cargo xwin build --target x86_64-pc-windows-msvc --features feat_windows
```

### macOS Cross-Compilation (osxcross)

- **Targets**: `x86_64-apple-darwin`, `aarch64-apple-darwin`
- **Toolchain**: osxcross with macOS SDK
- **Features**: Core Audio backend

```bash
# Manual osxcross usage
docker run --rm -v $(pwd):/root/src joseluisq/rust-linux-darwin-builder:1.88.0 \
  cargo build --target x86_64-apple-darwin --features feat_macos
```

### Linux Native Compilation

- **Target**: `x86_64-unknown-linux-gnu`
- **Audio Support**: PipeWire, PulseAudio, ALSA
- **Device Access**: `/dev/snd` mounted for audio testing

## Advanced Usage

### Custom Test Scripts

You can run custom test scripts in the containers:

```bash
# Run custom script in unified container
docker-compose -f docker-compose.unified.yml run --rm rsac-unified bash -c "your-custom-script.sh"

# Run interactive development session
docker-compose -f docker-compose.unified.yml run --rm rsac-dev
```

### Environment Variables

Key environment variables for customization:

- `RUST_BACKTRACE=1`: Enable Rust backtraces
- `RUST_LOG=debug`: Set logging level
- `XWIN_CACHE_DIR`: cargo-xwin cache directory
- `CARGO_NET_GIT_FETCH_WITH_CLI=true`: Use git CLI for fetching

### Volume Mounts

The containers use several volume mounts for efficiency:

- `cargo-cache`: Shared Cargo registry cache
- `target-cache`: Shared target directory cache
- `xwin-cache`: cargo-xwin SDK cache

## Troubleshooting

### Common Issues

1. **Docker Build Failures**
   ```bash
   # Clean and rebuild
   make docker-clean
   make docker-build
   ```

2. **Permission Issues**
   ```bash
   # Fix permissions on test results
   sudo chown -R $USER:$USER test-results/
   ```

3. **Audio Device Access (Linux)**
   ```bash
   # Ensure audio group membership
   sudo usermod -a -G audio $USER
   ```

### Debug Mode

Enable debug logging for troubleshooting:

```bash
# Set debug environment
export RUST_LOG=debug
export RUST_BACKTRACE=full

# Run with debug output
make docker-test-all
```

## Performance Optimization

### Build Cache

The setup uses Docker volumes for caching:

- Cargo registry cache is shared across containers
- Target directory cache speeds up rebuilds
- cargo-xwin SDK cache avoids re-downloading Windows SDKs

### Parallel Builds

For faster builds, you can run platform tests in parallel:

```bash
# Run platforms in parallel (requires sufficient resources)
make docker-test-linux &
make docker-test-windows &
make docker-test-macos &
wait
```

## Integration with CI/CD

The Docker setup is designed to work with CI/CD systems:

```yaml
# Example GitHub Actions integration
- name: Run Docker Tests
  run: |
    make docker-test-all
    make docker-dashboard
    
- name: Upload Test Results
  uses: actions/upload-artifact@v3
  with:
    name: test-results
    path: test-results/
```

## Contributing

When adding new platforms or features:

1. Add new Dockerfile in appropriate `docker/` subdirectory
2. Create platform-specific test script
3. Update `docker-compose.unified.yml`
4. Add new Make targets
5. Update this documentation

## References

- [cargo-xwin](https://github.com/rust-cross/cargo-xwin) - Windows cross-compilation
- [osxcross](https://github.com/tpoechtrager/osxcross) - macOS cross-compilation
- [joseluisq/rust-linux-darwin-builder](https://github.com/joseluisq/rust-linux-darwin-builder) - Multi-platform Rust builder
