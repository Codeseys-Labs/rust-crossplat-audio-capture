# =============================================================================
# Local audio capture testing for Windows (WASAPI)
# =============================================================================
#
# Tests all 3 capture tiers: system, device, process/tree
#
# Prerequisites:
#   - Windows 10 build 20348+ or Windows 11 (for process loopback)
#   - Audio render endpoint available (real or virtual like Scream/VB-Cable)
#   - AudioSrv (Windows Audio) service running
#   - Rust toolchain installed
#
# Usage:
#   .\scripts\test-audio-windows.ps1                    # Run all tiers
#   .\scripts\test-audio-windows.ps1 -Tier system       # Run only system tier
#   .\scripts\test-audio-windows.ps1 -Tier device       # Run only device tier
#   .\scripts\test-audio-windows.ps1 -Tier process      # Run only process tier
#   .\scripts\test-audio-windows.ps1 -Verbose           # Extra diagnostic output
#
# Exit codes:
#   0 - all requested tests passed
#   1 - one or more tests failed
#   2 - missing prerequisites
# =============================================================================

[CmdletBinding()]
param(
    [ValidateSet("all", "system", "device", "process")]
    [string]$Tier = "all",

    [switch]$Verbose
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# Globals
# ---------------------------------------------------------------------------

$script:PassCount = 0
$script:FailCount = 0
$script:SkipCount = 0
$script:Results = @()
$script:PlayerProcess = $null
$script:TestWav = $null

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

function Write-Header([string]$Text) {
    Write-Host ""
    Write-Host ("=" * 60) -ForegroundColor White
    Write-Host "  $Text" -ForegroundColor White
    Write-Host ("=" * 60) -ForegroundColor White
}

function Write-Info([string]$Msg)  { Write-Host "[INFO]  $Msg" -ForegroundColor Blue }
function Write-Ok([string]$Msg)    { Write-Host "[PASS]  $Msg" -ForegroundColor Green }
function Write-Fail([string]$Msg)  { Write-Host "[FAIL]  $Msg" -ForegroundColor Red }
function Write-Warn([string]$Msg)  { Write-Host "[WARN]  $Msg" -ForegroundColor Yellow }
function Write-Skip([string]$Msg)  { Write-Host "[SKIP]  $Msg" -ForegroundColor Cyan }

function Record-Result([string]$TierName, [string]$Name, [string]$Status, [string]$Detail = "") {
    $entry = "$Status  $TierName :: $Name"
    if ($Detail) { $entry += " - $Detail" }
    $script:Results += $entry

    switch ($Status) {
        "PASS" { $script:PassCount++; Write-Ok "$TierName :: $Name" }
        "FAIL" { $script:FailCount++; Write-Fail "$TierName :: $Name$(if ($Detail) { " - $Detail" })" }
        "SKIP" { $script:SkipCount++; Write-Skip "$TierName :: $Name$(if ($Detail) { " - $Detail" })" }
    }
}

# Run a single cargo test, record the result.
function Invoke-CargoTest([string]$TierName, [string]$Label, [string]$Filter) {
    Write-Info "Running: cargo test --test ci_audio --features feat_windows -- $Filter --nocapture"

    $env:RSAC_CI_AUDIO_AVAILABLE = "1"
    $output = & cargo test --test ci_audio --features feat_windows -- $Filter --nocapture 2>&1
    $exitCode = $LASTEXITCODE

    foreach ($line in $output) {
        Write-Host "    $line"
    }

    if ($exitCode -eq 0) {
        Record-Result $TierName $Label "PASS"
    } else {
        Record-Result $TierName $Label "FAIL" "exit code $exitCode"
    }
    return $exitCode
}

# Generate a test WAV file using PowerShell + .NET
function New-TestWav([string]$Path, [int]$DurationSecs = 5, [int]$SampleRate = 48000, [int]$Channels = 2) {
    # Generate a 440 Hz sine wave WAV file
    $frequency = 440.0
    $amplitude = 0.8
    $bitsPerSample = 16
    $numSamples = $SampleRate * $DurationSecs
    $blockAlign = $Channels * ($bitsPerSample / 8)
    $byteRate = $SampleRate * $blockAlign
    $dataSize = $numSamples * $blockAlign

    $stream = [System.IO.File]::Create($Path)
    $writer = New-Object System.IO.BinaryWriter($stream)

    # RIFF header
    $writer.Write([System.Text.Encoding]::ASCII.GetBytes("RIFF"))
    $writer.Write([int]($dataSize + 36))  # file size - 8
    $writer.Write([System.Text.Encoding]::ASCII.GetBytes("WAVE"))

    # fmt chunk
    $writer.Write([System.Text.Encoding]::ASCII.GetBytes("fmt "))
    $writer.Write([int]16)               # chunk size
    $writer.Write([int16]1)              # PCM format
    $writer.Write([int16]$Channels)
    $writer.Write([int]$SampleRate)
    $writer.Write([int]$byteRate)
    $writer.Write([int16]$blockAlign)
    $writer.Write([int16]$bitsPerSample)

    # data chunk
    $writer.Write([System.Text.Encoding]::ASCII.GetBytes("data"))
    $writer.Write([int]$dataSize)

    for ($i = 0; $i -lt $numSamples; $i++) {
        $t = $i / $SampleRate
        $sample = [Math]::Sin(2.0 * [Math]::PI * $frequency * $t) * $amplitude
        $int16Sample = [int16]([Math]::Max(-32768, [Math]::Min(32767, $sample * 32767)))
        for ($ch = 0; $ch -lt $Channels; $ch++) {
            $writer.Write($int16Sample)
        }
    }

    $writer.Close()
    $stream.Close()
}

# Start a background audio player process, return the Process object.
function Start-TestPlayer([string]$WavPath) {
    # Use PowerShell SoundPlayer in a child process so we get a PID.
    # SoundPlayer.PlaySync() blocks, so we run it in a background job via Start-Process.
    $script:PlayerProcess = Start-Process -FilePath "powershell.exe" -ArgumentList @(
        "-NoProfile", "-Command",
        "Add-Type -AssemblyName System.Windows.Forms; `$p = New-Object System.Media.SoundPlayer('$WavPath'); `$p.PlayLooping(); Start-Sleep -Seconds 30"
    ) -PassThru -WindowStyle Hidden

    Write-Info "Started audio player PID=$($script:PlayerProcess.Id)"
    Start-Sleep -Seconds 1  # Let audio start flowing
}

function Stop-TestPlayer {
    if ($script:PlayerProcess -and -not $script:PlayerProcess.HasExited) {
        try {
            $script:PlayerProcess.Kill()
            $script:PlayerProcess.WaitForExit(5000) | Out-Null
        } catch {
            # Already exited
        }
        Write-Info "Stopped test audio player"
    }
    $script:PlayerProcess = $null
}

# ---------------------------------------------------------------------------
# Cleanup handler (registered via try/finally in Main)
# ---------------------------------------------------------------------------

function Invoke-Cleanup {
    Write-Info "Cleaning up..."
    Stop-TestPlayer
    if ($script:TestWav -and (Test-Path $script:TestWav)) {
        Remove-Item -Force $script:TestWav -ErrorAction SilentlyContinue
        Write-Info "Removed temp WAV"
    }
}

# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------

function Test-Prerequisites {
    Write-Header "Prerequisite Checks"

    # 1. Windows Audio service
    $audioSrv = Get-Service -Name "AudioSrv" -ErrorAction SilentlyContinue
    if (-not $audioSrv) {
        Write-Fail "Windows Audio service (AudioSrv) not found."
        exit 2
    }
    if ($audioSrv.Status -ne "Running") {
        Write-Warn "AudioSrv is not running — attempting to start..."
        try {
            Start-Service -Name "AudioSrv"
            Start-Sleep -Seconds 2
            $audioSrv = Get-Service -Name "AudioSrv"
            if ($audioSrv.Status -eq "Running") {
                Write-Ok "AudioSrv started successfully"
            } else {
                Write-Fail "Could not start AudioSrv (status: $($audioSrv.Status))"
                exit 2
            }
        } catch {
            Write-Fail "Failed to start AudioSrv: $_"
            exit 2
        }
    } else {
        Write-Ok "AudioSrv is running"
    }

    # 2. Audio render endpoints
    $endpoints = $null
    try {
        Add-Type -AssemblyName System.Windows.Forms 2>$null
        $devices = Get-CimInstance -ClassName Win32_SoundDevice -ErrorAction SilentlyContinue
        if ($devices -and $devices.Count -gt 0) {
            Write-Ok "Found $($devices.Count) audio device(s) via WMI"
            if ($Verbose) {
                foreach ($d in $devices) {
                    Write-Info "  - $($d.Name) (Status: $($d.Status))"
                }
            }
        } else {
            Write-Warn "No audio devices found via WMI — tests may fail"
        }
    } catch {
        Write-Warn "Could not enumerate audio devices via WMI: $_"
    }

    # 3. Windows build version (process loopback requires 20348+)
    $buildNumber = [int](Get-CimInstance Win32_OperatingSystem).BuildNumber
    Write-Info "Windows build: $buildNumber"
    if ($buildNumber -ge 20348) {
        Write-Ok "Windows build $buildNumber supports process loopback"
    } else {
        Write-Warn "Windows build $buildNumber may not support process loopback (need 20348+)"
        Write-Warn "Process/application capture tests may be skipped"
    }

    # 4. Rust toolchain
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Fail "cargo not found in PATH. Install Rust: https://rustup.rs"
        exit 2
    }
    Write-Ok "Rust toolchain available"

    # 5. Compilation check
    Write-Info "Checking compilation..."
    $checkOutput = & cargo check --features feat_windows 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Fail "cargo check --features feat_windows failed"
        foreach ($line in $checkOutput | Select-Object -Last 5) {
            Write-Host "    $line"
        }
        exit 2
    }
    Write-Ok "Project compiles with feat_windows"
}

# ---------------------------------------------------------------------------
# Test WAV setup
# ---------------------------------------------------------------------------

function Initialize-TestEnvironment {
    Write-Header "Test Environment Setup"

    $script:TestWav = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), "rsac_test_tone_$(Get-Random).wav")
    New-TestWav -Path $script:TestWav -DurationSecs 5 -SampleRate 48000 -Channels 2

    if (Test-Path $script:TestWav) {
        $size = (Get-Item $script:TestWav).Length
        if ($size -gt 44) {
            Write-Ok "Generated test WAV: $script:TestWav ($size bytes)"
        } else {
            Write-Fail "Generated WAV file is too small ($size bytes)"
            exit 2
        }
    } else {
        Write-Fail "Failed to generate test WAV file"
        exit 2
    }
}

# ============================================================================
#  TIER 1: System Capture
# ============================================================================

function Invoke-SystemTests {
    Write-Header "Tier 1: System Capture"

    # Start background audio player
    Start-TestPlayer $script:TestWav

    $tierFailed = 0

    # System capture integration tests
    $r = Invoke-CargoTest "system" "system_capture_receives_audio" "test_system_capture_receives_audio"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "system" "capture_format_correct" "test_capture_format_correct"
    if ($r -ne 0) { $tierFailed++ }

    # Stream lifecycle tests
    $r = Invoke-CargoTest "system" "stream_start_read_stop" "test_stream_start_read_stop"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "system" "stream_stop_idempotent" "test_stream_stop_idempotent"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "system" "drop_while_running" "test_drop_while_running"
    if ($r -ne 0) { $tierFailed++ }

    # Platform capabilities (no audio hardware needed)
    $r = Invoke-CargoTest "system" "capabilities_query" "test_capabilities_query"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "system" "backend_name_matches_platform" "test_backend_name_matches_platform"
    if ($r -ne 0) { $tierFailed++ }

    Stop-TestPlayer
    return $tierFailed
}

# ============================================================================
#  TIER 2: Device Capture
# ============================================================================

function Invoke-DeviceTests {
    Write-Header "Tier 2: Device Capture"

    # Start background audio player
    Start-TestPlayer $script:TestWav

    $tierFailed = 0

    # Device enumeration
    $r = Invoke-CargoTest "device" "enumerate_devices_finds_at_least_one" "test_enumerate_devices_finds_at_least_one"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "device" "default_device_exists" "test_default_device_exists"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "device" "platform_capabilities_reasonable" "test_platform_capabilities_reasonable"
    if ($r -ne 0) { $tierFailed++ }

    # Device capture tests
    $r = Invoke-CargoTest "device" "capture_from_selected_device" "test_capture_from_selected_device"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "device" "all_enumerated_devices_have_valid_ids" "test_all_enumerated_devices_have_valid_ids"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "device" "capture_nonexistent_device" "test_capture_nonexistent_device"
    if ($r -ne 0) { $tierFailed++ }

    Stop-TestPlayer
    return $tierFailed
}

# ============================================================================
#  TIER 3: Process / Application / Tree Capture
# ============================================================================

function Invoke-ProcessTests {
    Write-Header "Tier 3: Process / Application / Tree Capture"

    # Check Windows build for process loopback support
    $buildNumber = [int](Get-CimInstance Win32_OperatingSystem).BuildNumber
    if ($buildNumber -lt 20348) {
        Write-Warn "Windows build $buildNumber does not support process loopback"
        Record-Result "process" "all process tests" "SKIP" "requires Windows build 20348+"
        return 0
    }

    # Tests spawn their own audio players, so no background player needed here.
    $tierFailed = 0

    # Application capture tests
    $r = Invoke-CargoTest "process" "app_capture_by_process_id" "test_app_capture_by_process_id"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "process" "app_capture_nonexistent_target" "test_app_capture_nonexistent_target"
    if ($r -ne 0) { $tierFailed++ }

    # Process tree capture tests
    $r = Invoke-CargoTest "process" "process_tree_capture_receives_audio" "test_process_tree_capture_receives_audio"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "process" "process_tree_capture_nonexistent_pid" "test_process_tree_capture_nonexistent_pid"
    if ($r -ne 0) { $tierFailed++ }

    $r = Invoke-CargoTest "process" "process_tree_capture_lifecycle" "test_process_tree_capture_lifecycle"
    if ($r -ne 0) { $tierFailed++ }

    return $tierFailed
}

# ============================================================================
#  Main
# ============================================================================

try {
    Test-Prerequisites
    Initialize-TestEnvironment

    $totalFailed = 0

    switch ($Tier) {
        "system" {
            $totalFailed += Invoke-SystemTests
        }
        "device" {
            $totalFailed += Invoke-DeviceTests
        }
        "process" {
            $totalFailed += Invoke-ProcessTests
        }
        "all" {
            $totalFailed += Invoke-SystemTests
            $totalFailed += Invoke-DeviceTests
            $totalFailed += Invoke-ProcessTests
        }
    }

    # ========================================================================
    #  Summary
    # ========================================================================

    Write-Header "Test Summary"
    Write-Host ""

    foreach ($r in $script:Results) {
        if ($r.StartsWith("PASS")) {
            Write-Host "  $r" -ForegroundColor Green
        } elseif ($r.StartsWith("FAIL")) {
            Write-Host "  $r" -ForegroundColor Red
        } elseif ($r.StartsWith("SKIP")) {
            Write-Host "  $r" -ForegroundColor Cyan
        }
    }

    Write-Host ""
    $total = $script:PassCount + $script:FailCount + $script:SkipCount
    Write-Host "  Total: $total  |  " -NoNewline
    Write-Host "Passed: $($script:PassCount)  |  " -ForegroundColor Green -NoNewline
    Write-Host "Failed: $($script:FailCount)  |  " -ForegroundColor Red -NoNewline
    Write-Host "Skipped: $($script:SkipCount)" -ForegroundColor Cyan
    Write-Host ""

    if ($script:FailCount -gt 0) {
        Write-Fail "$($script:FailCount) test(s) failed."
        exit 1
    } else {
        Write-Ok "All tests passed."
        exit 0
    }
}
finally {
    Invoke-Cleanup
}
