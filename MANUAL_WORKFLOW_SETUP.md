# Manual GitHub Actions Workflow Setup

Due to OAuth scope limitations, the GitHub Actions workflow files need to be added manually through the GitHub web interface or with proper authentication.

## ✅ What's Already Pushed

The following has been successfully pushed to the `refactor` branch:
- ✅ All test examples using real library APIs
- ✅ Comprehensive testing infrastructure
- ✅ Documentation and validation scripts
- ✅ Updated Cargo.toml with new examples
- ✅ Enhanced Docker setup

## 🔧 Workflow Files That Need Manual Addition

### 1. Enhanced Existing Workflows

The following existing workflow files need to be updated with the enhanced configurations:

#### `.github/workflows/ci.yml` (lines 47-60)
Replace the Linux audio libraries installation section with:
```yaml
      - name: Install audio dev libraries (Linux)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            libpipewire-0.3-dev \
            libspa-0.2-dev \
            libpulse-dev \
            libasound2-dev \
            pkg-config \
            pipewire \
            pipewire-audio-client-libraries \
            pulseaudio \
            firefox
```

Add after line 73 (Build step):
```yaml
      - name: Build examples
        run: cargo build --examples --all-features --verbose
```

#### `.github/workflows/linux.yml` (around line 269)
Add before the existing test capture steps:
```yaml
          # Test library functionality
          echo "Testing library functionality..."
          cargo run --example demo_library -- --list-only --verbose
```

Update the test capture commands to include `--verbose` flag.

#### `.github/workflows/macos.yml` (around line 103)
Add before the existing test capture steps:
```yaml
          # Test library functionality
          echo "Testing library functionality..."
          cargo run --example demo_library -- --list-only --verbose
```

Update the test capture commands to include `--verbose` flag.

#### `.github/workflows/windows.yml` (around line 88)
Add before the existing test capture steps:
```yaml
              # Test library functionality
              Write-Host "Testing library functionality..."
              cargo run --example demo_library -- --list-only --verbose
```

Update the test capture commands to include `--verbose` flag.

### 2. New Comprehensive Workflow

Create a new file `.github/workflows/audio-tests.yml` with the following content:

```yaml
name: Cross-Platform Audio Tests

on:
  push:
    branches: ["main", "master"]
  pull_request:
    branches: ["main", "master"]
  workflow_dispatch:
    inputs:
      debug_enabled:
        description: "Enable debug output"
        required: false
        type: boolean
        default: false
      test_platform:
        description: "Platform to test (all, linux, macos, windows)"
        required: false
        type: choice
        options:
          - all
          - linux
          - macos
          - windows
        default: all

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  # Linux audio tests with both PipeWire and PulseAudio
  linux-audio-tests:
    if: ${{ github.event.inputs.test_platform == 'all' || github.event.inputs.test_platform == 'linux' || github.event.inputs.test_platform == '' }}
    uses: ./.github/workflows/linux.yml
    with:
      debug_enabled: ${{ github.event.inputs.debug_enabled == 'true' }}
      audio_backend: "auto"

  # macOS audio tests with CoreAudio
  macos-audio-tests:
    if: ${{ github.event.inputs.test_platform == 'all' || github.event.inputs.test_platform == 'macos' || github.event.inputs.test_platform == '' }}
    uses: ./.github/workflows/macos.yml
    with:
      debug_enabled: ${{ github.event.inputs.debug_enabled == 'true' }}

  # Windows audio tests with WASAPI
  windows-audio-tests:
    if: ${{ github.event.inputs.test_platform == 'all' || github.event.inputs.test_platform == 'windows' || github.event.inputs.test_platform == '' }}
    uses: ./.github/workflows/windows.yml
    with:
      debug_enabled: ${{ github.event.inputs.debug_enabled == 'true' }}

  # Collect and analyze results
  analyze-results:
    needs: [linux-audio-tests, macos-audio-tests, windows-audio-tests]
    if: always()
    runs-on: ubuntu-latest
    steps:
      - name: Checkout code
        uses: actions/checkout@v4

      - name: Download all artifacts
        uses: actions/download-artifact@v4
        with:
          path: test-results

      - name: Analyze test results
        run: |
          echo "=== Cross-Platform Audio Test Results ==="
          
          # Check Linux results
          if [ -d "test-results/linux-pipewire-audio-test-results" ]; then
            echo "✅ Linux PipeWire tests completed"
            ls -la test-results/linux-pipewire-audio-test-results/
          else
            echo "❌ Linux PipeWire tests failed or skipped"
          fi
          
          if [ -d "test-results/linux-pulseaudio-audio-test-results" ]; then
            echo "✅ Linux PulseAudio tests completed"
            ls -la test-results/linux-pulseaudio-audio-test-results/
          else
            echo "❌ Linux PulseAudio tests failed or skipped"
          fi
          
          # Check macOS results
          if [ -d "test-results/macos-audio-test-results" ]; then
            echo "✅ macOS CoreAudio tests completed"
            ls -la test-results/macos-audio-test-results/
          else
            echo "❌ macOS CoreAudio tests failed or skipped"
          fi
          
          # Check Windows results
          if [ -d "test-results/windows-audio-test-results" ]; then
            echo "✅ Windows WASAPI tests completed"
            ls -la test-results/windows-audio-test-results/
          else
            echo "❌ Windows WASAPI tests failed or skipped"
          fi
          
          echo "=== Test Summary ==="
          find test-results -name "*.wav" -exec echo "Audio file: {}" \; -exec ls -lh {} \;

      - name: Upload combined results
        uses: actions/upload-artifact@v4
        with:
          name: cross-platform-audio-test-results
          path: test-results/
          retention-days: 7
```

## 🚀 How to Apply These Changes

### Option 1: Manual GitHub Web Interface
1. Go to your repository on GitHub
2. Navigate to each workflow file
3. Click "Edit" and apply the changes above
4. Commit directly to the branch

### Option 2: Local Git with Proper Authentication
1. Set up a Personal Access Token with `workflow` scope
2. Use `git` with the token to push workflow changes

### Option 3: Merge to Master Branch
1. Create a pull request from `refactor` to `master`
2. The workflows will run on the `master` branch after merge

## 🧪 Testing the Setup

Once the workflows are updated:

1. **Automatic Testing**: Push to main/master branch
2. **Manual Testing**: Use the "Actions" tab → "Cross-Platform Audio Tests" → "Run workflow"
3. **Local Testing**: Run `./scripts/test_ci_locally.sh`

## 📊 Expected Results

The enhanced CI/CD will:
- ✅ Test real library APIs instead of simulated data
- ✅ Validate device enumeration on all platforms
- ✅ Test application-specific capture
- ✅ Verify audio content with frequency analysis
- ✅ Provide comprehensive test artifacts

The workflows will now properly test your actual cross-platform audio capture library functionality!
