# Windows Audio Debug Workflow

This document describes the Windows Audio Debug workflow designed to test virtual audio driver installation and VLC audio capture in CI environments.

## Overview

The Windows Audio Debug workflow (`windows-audio-debug.yml`) is a comprehensive testing tool that:

1. **Analyzes the current audio system state** before any modifications
2. **Downloads and installs a virtual audio driver** to provide audio devices in headless environments
3. **Tests VLC Media Player** with the virtual audio devices
4. **Runs our Rust audio capture code** to test WASAPI application capture
5. **Provides detailed diagnostics** and logs for troubleshooting

## Purpose

This workflow helps us understand:
- Whether virtual audio drivers solve the VLC audio issues in CI
- How to properly set up audio capture in headless Windows environments
- If our WASAPI code works correctly with virtual audio devices
- What the best approach is for automated audio testing

## Usage

### Manual Trigger

The workflow is designed to be triggered manually via GitHub Actions:

1. Go to the **Actions** tab in your GitHub repository
2. Select **"Windows Audio Debug - Virtual Driver Testing"**
3. Click **"Run workflow"**
4. Configure options:
   - **Debug level**: `basic`, `detailed`, or `verbose`
   - **Test duration**: How long to run VLC (in seconds, default: 10)

### Debug Levels

- **`basic`**: Essential diagnostics only
- **`detailed`**: Comprehensive audio system analysis (default)
- **`verbose`**: Full system information including all PnP devices

## What It Tests

### 1. Audio System Analysis
- Windows Audio Service status
- Available audio devices (before and after driver installation)
- PnP device enumeration
- Virtual audio device detection

### 2. Virtual Audio Driver Installation
- Downloads the latest Virtual-Audio-Driver from GitHub
- Attempts installation using PnPUtil
- Restarts Windows Audio Service to detect new devices
- Verifies virtual device installation

### 3. VLC Media Player Testing
- Downloads and installs VLC Media Player
- Tests VLC audio output with virtual devices
- Generates detailed VLC debug logs
- Analyzes audio device usage and errors

### 4. Rust Audio Capture Testing
- Builds our Rust audio capture components
- Runs the `dynamic_vlc_capture` binary
- Tests WASAPI application capture with VLC
- Captures detailed error information

## Expected Outcomes

### Success Scenario
If virtual audio drivers work correctly, we should see:
- Virtual audio devices detected in system
- VLC successfully outputting to virtual device
- Our Rust capture successfully capturing VLC audio
- Generated audio capture file

### Failure Scenarios
Common issues and their meanings:

1. **Driver Installation Fails**
   - CI environments may block unsigned drivers
   - May need alternative virtual audio solutions

2. **VLC Cannot Find Audio Devices**
   - Virtual driver not properly installed
   - Windows Audio Service issues
   - Need different driver or approach

3. **WASAPI Capture Fails**
   - Virtual devices may not support application capture
   - Need to test different virtual audio solutions

## Artifacts

The workflow uploads debug artifacts including:
- `vlc_debug.log` - Detailed VLC audio diagnostics
- `*.wav` - Any generated audio capture files
- `*.log` - Additional debug logs

## Virtual Audio Driver

The workflow uses the **Virtual-Audio-Driver** project:
- **Repository**: https://github.com/VirtualDrivers/Virtual-Audio-Driver
- **Purpose**: Creates virtual speaker and microphone devices
- **Compatibility**: Windows 10/11, signed drivers available
- **Features**: Designed for headless servers and CI environments

## Troubleshooting

### Common Issues

1. **"Driver installation failed"**
   - Expected in CI due to driver signing restrictions
   - Try alternative virtual audio solutions (VB-Audio, etc.)

2. **"VLC audio output errors"**
   - Check if virtual devices are properly detected
   - Verify Windows Audio Service is running

3. **"WASAPI capture device in use"**
   - Indicates our process detection logic is working
   - May need to test with different audio sources

### Manual Testing

You can also run the PowerShell script locally:

```powershell
# Run the virtual audio test script
.\scripts\test-virtual-audio-windows.ps1 -TestDuration 15 -Verbose

# Skip driver installation if already installed
.\scripts\test-virtual-audio-windows.ps1 -SkipDriverInstall -TestDuration 10
```

## Next Steps

Based on the results of this debug workflow, we can:

1. **If virtual drivers work**: Integrate them into our regular CI
2. **If virtual drivers fail**: Explore alternative solutions (VB-Audio, etc.)
3. **If capture works**: Validate our WASAPI implementation
4. **If capture fails**: Debug and improve our audio capture logic

## Related Files

- `.github/workflows/windows-audio-debug.yml` - Main debug workflow
- `scripts/test-virtual-audio-windows.ps1` - PowerShell testing script
- `src/bin/dynamic_vlc_capture.rs` - Rust audio capture binary
- `scripts/test-vlc-capture-windows.ps1` - Original VLC test script

## Contributing

If you discover issues or improvements for the debug workflow:

1. Run the debug workflow and collect artifacts
2. Analyze the logs for specific error patterns
3. Propose improvements to the workflow or scripts
4. Test alternative virtual audio solutions

This debug workflow is a learning tool to help us understand Windows audio capture in CI environments and improve our testing strategy.
