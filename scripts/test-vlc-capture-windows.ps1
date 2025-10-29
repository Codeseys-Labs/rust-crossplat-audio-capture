# VLC Audio Capture Test Script for Windows
# This script tests our audio capture library with VLC streaming audio

param(
    [switch]$Debug = $false
)

# Global variables for tracking
$script:CleanupDone = $false
$script:ScriptStartTime = Get-Date
$script:ProcessLog = "process_tracking.log"
$script:VlcProcess = $null

# Initialize process tracking log
"=== Process Tracking Log - $(Get-Date) ===" | Out-File -FilePath $script:ProcessLog -Encoding UTF8
"Script PID: $PID" | Out-File -FilePath $script:ProcessLog -Append -Encoding UTF8

# Function to log process events
function Log-ProcessEvent {
    param(
        [string]$Event,
        [string]$Details
    )
    $timestamp = Get-Date -Format "HH:mm:ss.fff"
    $logEntry = "[$timestamp] $Event`: $Details"
    $logEntry | Out-File -FilePath $script:ProcessLog -Append -Encoding UTF8
    Write-Host $logEntry
}

# Function to log child processes
function Log-ChildProcesses {
    param(
        [int]$ParentPid,
        [string]$Label
    )
    Log-ProcessEvent "CHILD_SCAN" "$Label - scanning children of PID $ParentPid"
    try {
        Get-WmiObject Win32_Process | Where-Object { $_.ParentProcessId -eq $ParentPid } | 
            ForEach-Object { "$($_.ProcessId) $($_.Name) $($_.CommandLine)" } |
            Out-File -FilePath $script:ProcessLog -Append -Encoding UTF8
    } catch {
        "Failed to get child processes: $($_.Exception.Message)" | Out-File -FilePath $script:ProcessLog -Append -Encoding UTF8
    }
}

# Function to print colored output
function Write-Status {
    param(
        [string]$Status,
        [string]$Message
    )
    switch ($Status) {
        "OK" { Write-Host "[OK] $Message" -ForegroundColor Green }
        "WARN" { Write-Host "[WARN] $Message" -ForegroundColor Yellow }
        "ERROR" { Write-Host "[ERROR] $Message" -ForegroundColor Red }
        "INFO" { Write-Host "[INFO] $Message" -ForegroundColor Blue }
        default { Write-Host $Message }
    }
}

Write-Host "=== VLC Audio Capture Test Script for Windows ==="

# Test URLs (reliable audio sources)
$TestUrls = @(
    "https://www.soundjay.com/misc/sounds/bell-ringing-05.wav",
    "https://file-examples.com/storage/fe68c1b7c1a9fd42d99c603/2017/11/file_example_WAV_1MG.wav",
    "https://www.kozco.com/tech/LRMonoPhase4.wav",
    "https://www.kozco.com/tech/piano2.wav"
)

# Function to test URL accessibility
function Test-Url {
    param([string]$Url)
    Write-Status "INFO" "Testing URL: $Url"
    
    try {
        $response = Invoke-WebRequest -Uri $Url -Method Head -TimeoutSec 10 -ErrorAction Stop
        if ($response.StatusCode -eq 200) {
            Write-Status "OK" "URL is accessible"
            return $true
        }
    } catch {
        Write-Status "WARN" "URL not accessible or timed out: $($_.Exception.Message)"
    }
    return $false
}

# Find a working URL
$WorkingUrl = ""
Write-Status "INFO" "Searching for accessible audio URLs..."

foreach ($url in $TestUrls) {
    if (Test-Url $url) {
        $WorkingUrl = $url
        Write-Status "OK" "Found working URL: $WorkingUrl"
        break
    }
}

# Fallback: generate local test audio if no URL works
if (-not $WorkingUrl) {
    Write-Status "WARN" "No working URLs found, generating local test audio"
    
    # Create a simple test audio using ffmpeg (if available) or download a sample
    $testAudioFile = "vlc_test_audio.wav"
    
    if (Get-Command ffmpeg -ErrorAction SilentlyContinue) {
        # Generate test audio with ffmpeg - create a longer, more prominent tone
        & ffmpeg -f lavfi -i "sine=frequency=440:duration=30" -ar 48000 -ac 2 $testAudioFile -y
        Write-Status "OK" "Created local test audio with ffmpeg: $testAudioFile (30 second tone)"
    } else {
        # Try to download a sample file first
        try {
            Invoke-WebRequest -Uri "https://www.soundjay.com/misc/sounds/bell-ringing-05.wav" -OutFile $testAudioFile -TimeoutSec 30
            Write-Status "OK" "Downloaded test audio: $testAudioFile"
        } catch {
            Write-Status "WARN" "Failed to download test audio, creating minimal WAV file: $($_.Exception.Message)"
            
            # Final fallback: Create a minimal WAV file with PowerShell
            # This creates a very simple 1-second 440Hz tone
            try {
                $sampleRate = 44100
                $frequency = 440
                $duration = 10  # 10 seconds
                $amplitude = 0.3
                
                $samples = [System.Collections.Generic.List[byte]]::new()
                
                # WAV header (44 bytes)
                $samples.AddRange([System.Text.Encoding]::ASCII.GetBytes("RIFF"))
                $samples.AddRange([System.BitConverter]::GetBytes([int32](36 + $sampleRate * $duration * 2)))
                $samples.AddRange([System.Text.Encoding]::ASCII.GetBytes("WAVE"))
                $samples.AddRange([System.Text.Encoding]::ASCII.GetBytes("fmt "))
                $samples.AddRange([System.BitConverter]::GetBytes([int32]16))  # Subchunk1Size
                $samples.AddRange([System.BitConverter]::GetBytes([int16]1))   # AudioFormat (PCM)
                $samples.AddRange([System.BitConverter]::GetBytes([int16]1))   # NumChannels (mono)
                $samples.AddRange([System.BitConverter]::GetBytes([int32]$sampleRate))
                $samples.AddRange([System.BitConverter]::GetBytes([int32]($sampleRate * 2)))  # ByteRate
                $samples.AddRange([System.BitConverter]::GetBytes([int16]2))   # BlockAlign
                $samples.AddRange([System.BitConverter]::GetBytes([int16]16))  # BitsPerSample
                $samples.AddRange([System.Text.Encoding]::ASCII.GetBytes("data"))
                $samples.AddRange([System.BitConverter]::GetBytes([int32]($sampleRate * $duration * 2)))
                
                # Generate sine wave data
                for ($i = 0; $i -lt ($sampleRate * $duration); $i++) {
                    $sample = [Math]::Sin(2 * [Math]::PI * $frequency * $i / $sampleRate) * $amplitude * 32767
                    $samples.AddRange([System.BitConverter]::GetBytes([int16]$sample))
                }
                
                [System.IO.File]::WriteAllBytes($testAudioFile, $samples.ToArray())
                Write-Status "OK" "Created minimal WAV file with PowerShell: $testAudioFile"
            } catch {
                Write-Status "ERROR" "Failed to create any test audio: $($_.Exception.Message)"
                exit 1
            }
        }
    }
    
    $WorkingUrl = (Resolve-Path $testAudioFile).Path
}

Write-Status "INFO" "Final test URL: $WorkingUrl"

# Check VLC availability
$vlcPaths = @(
    "${env:ProgramFiles}\VideoLAN\VLC\vlc.exe",
    "${env:ProgramFiles(x86)}\VideoLAN\VLC\vlc.exe",
    "vlc.exe"
)

$vlcPath = $null
foreach ($path in $vlcPaths) {
    if (Test-Path $path -ErrorAction SilentlyContinue) {
        $vlcPath = $path
        break
    }
}

if (-not $vlcPath) {
    # Try to find VLC in PATH
    $vlcPath = Get-Command vlc -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source
}

if (-not $vlcPath) {
    Write-Status "ERROR" "VLC not found"
    Write-Status "INFO" "This test requires VLC to be installed"
    Write-Status "INFO" "Install from: https://www.videolan.org/vlc/"
    exit 1
}

Write-Status "OK" "VLC found: $vlcPath"

# Cleanup function
function Invoke-Cleanup {
    param([int]$ExitCode = 0)
    
    Log-ProcessEvent "CLEANUP_START" "Cleanup called with exit code: $ExitCode"
    
    # Prevent recursive cleanup calls
    if ($script:CleanupDone) {
        Log-ProcessEvent "CLEANUP_SKIP" "Cleanup already done, skipping"
        return
    }
    $script:CleanupDone = $true
    
    Write-Status "INFO" "Cleaning up..."
    Log-ChildProcesses $PID "Before cleanup"
    
    if ($script:VlcProcess -and -not $script:VlcProcess.HasExited) {
        Write-Status "INFO" "Stopping VLC (PID: $($script:VlcProcess.Id))"
        Log-ProcessEvent "VLC_STOP_START" "Stopping VLC PID: $($script:VlcProcess.Id)"
        
        try {
            $script:VlcProcess.CloseMainWindow()
            if (-not $script:VlcProcess.WaitForExit(5000)) {
                Log-ProcessEvent "VLC_FORCE_KILL" "VLC didn't exit gracefully, forcing kill"
                $script:VlcProcess.Kill()
            } else {
                Log-ProcessEvent "VLC_GRACEFUL_EXIT" "VLC exited gracefully"
            }
        } catch {
            Log-ProcessEvent "VLC_CLEANUP_ERROR" "Error during VLC cleanup: $($_.Exception.Message)"
        }
    } else {
        Log-ProcessEvent "VLC_NOT_RUNNING" "VLC not running or already exited"
    }
    
    # Clean up temporary files
    Log-ProcessEvent "FILE_CLEANUP" "Removing temporary files"
    Remove-Item -Path "vlc_test_audio.wav" -ErrorAction SilentlyContinue
    
    Log-ChildProcesses $PID "After cleanup"
    Log-ProcessEvent "CLEANUP_COMPLETE" "Cleanup completed, original exit code: $ExitCode"
}

# Set up cleanup on exit
Register-EngineEvent PowerShell.Exiting -Action { Invoke-Cleanup }

try {
    # Start VLC with the URL
    Write-Status "INFO" "Starting VLC with audio stream..."
    Log-ProcessEvent "VLC_START" "Starting VLC with URL: $WorkingUrl"
    
    # VLC arguments with volume control and audio settings
    # CRITICAL: Ensure VLC plays audio at maximum volume for Virtual Audio Driver capture
    $vlcArgs = @(
        "--intf", "dummy",           # No GUI interface
        "--loop",                    # Loop the audio indefinitely
        "--volume", "256",           # Set volume to maximum (256 = 100% in VLC)
        "--gain", "1.5",            # Increase audio gain for better capture
        "--audio-visual", "dummy",   # Disable visualizations
        "--no-video",               # Audio only
        "--aout", "waveout",         # Use WaveOut audio output for explicit device selection
        "--waveout-audio-device", "Virtual Audio Speaker", # Force output to the virtual device
        "--audio-replay-gain-mode", "none",  # Disable replay gain
        "--audio-replay-gain-preamp", "0",   # No preamp
        "--start-time", "0",        # Start from beginning
        "--repeat",                 # Repeat the playlist (ensures continuous playback)
        "--verbose", "2",           # Verbose logging
        $WorkingUrl
    )

    Write-Status "INFO" "VLC arguments: $($vlcArgs -join ' ')"

    # Skip VLC diagnostics for now as they may hang - focus on getting audio working

    $script:VlcProcess = Start-Process -FilePath $vlcPath -ArgumentList $vlcArgs -PassThru -RedirectStandardOutput "vlc_capture_test.log" -RedirectStandardError "vlc_capture_test_error.log"
    
    Log-ProcessEvent "VLC_STARTED" "VLC PID: $($script:VlcProcess.Id)"
    Log-ChildProcesses $PID "After VLC start"
    
    # Wait for VLC to start
    Write-Status "INFO" "Waiting for VLC to start..."
    Start-Sleep -Seconds 8
    
    # Check if VLC is running
    if ($script:VlcProcess.HasExited) {
        Write-Status "ERROR" "VLC failed to start"
        Write-Status "INFO" "VLC logs:"
        if (Test-Path "vlc_capture_test.log") {
            Get-Content "vlc_capture_test.log" | Write-Host
        }
        if (Test-Path "vlc_capture_test_error.log") {
            Get-Content "vlc_capture_test_error.log" | Write-Host
        }
        exit 1
    }
    
    Write-Status "OK" "VLC is running with PID: $($script:VlcProcess.Id)"

    # List all VLC instances for debugging
    Write-Status "INFO" "Listing all VLC instances..."
    $allVlcProcesses = Get-Process -Name "vlc" -ErrorAction SilentlyContinue
    if ($allVlcProcesses) {
        foreach ($vlc in $allVlcProcesses) {
            Write-Status "INFO" "VLC Process: PID=$($vlc.Id), CPU=$($vlc.CPU), Memory=$([math]::Round($vlc.WorkingSet64/1MB, 2))MB"

            # Try to get command line for this process
            try {
                $cmdLine = Get-WmiObject Win32_Process -Filter "ProcessId = $($vlc.Id)" | Select-Object -ExpandProperty CommandLine
                Write-Status "INFO" "  Command: $cmdLine"
            } catch {
                Write-Status "WARN" "  Could not get command line for PID $($vlc.Id)"
            }
        }
    } else {
        Write-Status "WARN" "No VLC processes found!"
    }

    # CRITICAL: Set Virtual Audio Driver as default audio device for VLC to use
    Write-Status "INFO" "Setting up Virtual Audio Driver as default audio device..."
    try {
        # List all audio devices first
        Write-Status "INFO" "Available audio devices:"
        $audioDevices = Get-WmiObject -Class Win32_SoundDevice
        foreach ($device in $audioDevices) {
            Write-Status "INFO" "  Audio Device: $($device.Name) - $($device.Description)"
        }

        # Try to set Virtual Audio Driver as default using PowerShell audio cmdlets
        # Note: This may require Windows 10/11 with audio management features
        try {
            # First try the AudioDeviceCmdlets module if available
            if (Get-Module -ListAvailable -Name AudioDeviceCmdlets) {
                Import-Module AudioDeviceCmdlets -ErrorAction SilentlyContinue
                $virtualDevice = Get-AudioDevice -List | Where-Object { 
                    $_.Name -like "*Virtual*Audio*" -or 
                    $_.Name -like "*Virtual*Speaker*" -or
                    $_.Name -like "*Virtual*Driver*"
                }
                if ($virtualDevice) {
                    Write-Status "OK" "Found Virtual Audio device: $($virtualDevice.Name)"
                    Set-AudioDevice -ID $virtualDevice.ID
                    Write-Status "OK" "Set Virtual Audio Driver as default audio device"
                } else {
                    Write-Status "WARN" "Virtual Audio device not found in audio device list"
                    # List all available audio devices for debugging
                    Get-AudioDevice -List | ForEach-Object {
                        Write-Status "INFO" "  Available: $($_.Name) (ID: $($_.ID))"
                    }
                }
            } else {
                Write-Status "INFO" "AudioDeviceCmdlets module not available, using alternative method"

                # Alternative: Verify Virtual Audio Driver device installation
                Write-Status "INFO" "Attempting to verify Virtual Audio Driver device installation..."

                # Look for Virtual Audio Driver in WMI query above
                $virtualFound = $false
                foreach ($device in $audioDevices) {
                    if ($device.Name -like "*Virtual*Audio*" -or 
                        $device.Description -like "*Virtual*Audio*" -or
                        $device.Name -like "*Virtual*Driver*") {
                        Write-Status "OK" "Virtual Audio device confirmed via WMI: $($device.Name)"
                        $virtualFound = $true
                        break
                    }
                }

                if ($virtualFound) {
                    Write-Status "OK" "Virtual Audio Driver is properly installed and detected"

                    # Try to get more details about the Virtual Audio device
                    try {
                        $virtualDetails = Get-WmiObject -Class Win32_SoundDevice | Where-Object { 
                            $_.Name -like "*Virtual*Audio*" -or $_.Name -like "*Virtual*Driver*" 
                        }
                        if ($virtualDetails) {
                            Write-Status "INFO" "Virtual Audio device details:"
                            Write-Status "INFO" "  Name: $($virtualDetails.Name)"
                            Write-Status "INFO" "  Status: $($virtualDetails.Status)"
                            Write-Status "INFO" "  DeviceID: $($virtualDetails.DeviceID)"
                        }
                    } catch {
                        Write-Status "WARN" "Could not get detailed Virtual Audio device info: $_"
                    }

                    # SIMPLIFIED: Focus on verifying Virtual Audio Driver exists and Windows Audio service
                    Write-Status "INFO" "Verifying Virtual Audio Driver and audio services..."
                    try {
                        # Ensure Windows Audio service is running
                        $audioService = Get-Service -Name "AudioSrv" -ErrorAction SilentlyContinue
                        if ($audioService -and $audioService.Status -eq "Running") {
                            Write-Status "OK" "Windows Audio service is running"
                        } else {
                            Write-Status "WARN" "Windows Audio service may not be running properly"
                        }

                        # Note: Relying on VLC WASAPI + default device approach
                        Write-Status "INFO" "Using WASAPI + default device strategy for Virtual Audio Driver routing"

                    } catch {
                        Write-Status "WARN" "Audio service check failed: $_"
                    }
                } else {
                    Write-Status "ERROR" "Virtual Audio Driver device not found in WMI audio devices!"
                    Write-Status "ERROR" "This means Virtual Audio Driver installation may have failed"
                }
            }
        } catch {
            Write-Status "WARN" "Could not set Virtual Audio Driver as default using PowerShell: $_"
        }
    } catch {
        Write-Status "WARN" "Audio device configuration failed: $_"
    }

    # Wait a bit more for VLC to start playing
    Write-Status "INFO" "Waiting for VLC to start playing audio..."
    Start-Sleep -Seconds 5

    # Test our audio capture
    Write-Status "INFO" "Testing audio capture with our library..."

    # Test: Dynamic VLC capture example
    Write-Status "INFO" "Running dynamic_vlc_capture example..."
    Log-ProcessEvent "CARGO_START" "Starting cargo run"
    Log-ChildProcesses $PID "Before cargo run"

    # Set environment variables
    $env:CI = "true"
    $env:GITHUB_ACTIONS = "true"

    # Run the capture test
    $cargoResult = & cargo run --bin dynamic_vlc_capture --no-default-features --features feat_windows 10 2>&1
    $cargoExitCode = $LASTEXITCODE
    
    # Save output to log file
    $cargoResult | Out-File -FilePath "flexible_test.log" -Encoding UTF8
    
    Log-ProcessEvent "CARGO_COMPLETE" "Cargo completed with exit code: $cargoExitCode"
    Log-ChildProcesses $PID "After cargo run"

    if ($cargoExitCode -ne 0) {
        Write-Status "ERROR" "Cargo command failed with exit code $cargoExitCode"
        Log-ProcessEvent "CARGO_ERROR" "Cargo failed with exit code: $cargoExitCode"
        Write-Host "=== Capture Logs ==="
        if (Test-Path "flexible_test.log") {
            Get-Content "flexible_test.log" | Write-Host
        }
        Write-Host "=== This is a FAILURE - audio capture did not succeed ==="
        exit 1
    } else {
        Log-ProcessEvent "CARGO_SUCCESS" "Cargo completed successfully"
        Write-Status "OK" "Audio capture completed successfully"
    }
    
    # Show logs for debugging
    Write-Status "INFO" "=== VLC Logs (first 30 lines) ==="
    if (Test-Path "vlc_capture_test.log") {
        Get-Content "vlc_capture_test.log" | Select-Object -First 30 | Write-Host
    }

    Write-Status "INFO" "=== VLC Error Logs ==="
    if (Test-Path "vlc_capture_test_error.log") {
        Get-Content "vlc_capture_test_error.log" | Write-Host
    }

    # Check if VLC is actually playing audio
    Write-Status "INFO" "=== VLC Audio Status Check ==="
    if (Test-Path "vlc_capture_test.log") {
        $vlcLogs = Get-Content "vlc_capture_test.log" -Raw

        if ($vlcLogs -match "audio output") {
            Write-Status "OK" "VLC audio output detected in logs"
        } else {
            Write-Status "WARN" "No audio output detected in VLC logs"
        }

        if ($vlcLogs -match "main audio output") {
            Write-Status "OK" "VLC main audio output initialized"
        } else {
            Write-Status "WARN" "VLC main audio output not found in logs"
        }

        if ($vlcLogs -match "volume") {
            Write-Status "OK" "VLC volume settings found in logs"
        } else {
            Write-Status "WARN" "No volume settings found in VLC logs"
        }
    }
    
    Write-Status "INFO" "=== Dynamic VLC Capture Logs ==="
    if (Test-Path "flexible_test.log") {
        Get-Content "flexible_test.log" | Write-Host
    }
    
    Write-Status "INFO" "VLC Audio Capture Test Complete"

    # Summary and Validation
    Write-Host ""
    Write-Host "=== Test Summary ==="
    Write-Host "VLC URL: $WorkingUrl"
    Write-Host "VLC PID: $($script:VlcProcess.Id)"

    # Validate the output file
    $captureFile = "dynamic_vlc_capture.wav"
    if (Test-Path $captureFile) {
        $fileSize = (Get-Item $captureFile).Length
        Write-Host "Dynamic Capture File: Created ($fileSize bytes)"

        # Check if file is suspiciously small (less than 1KB indicates failure)
        if ($fileSize -lt 1024) {
            Write-Status "ERROR" "WAV file is too small ($fileSize bytes) - likely empty or corrupt"
            Write-Status "ERROR" "This indicates the audio capture failed or captured only silence"
            exit 1
        } else {
            Write-Status "OK" "WAV file size is reasonable ($fileSize bytes)"
        }
    } else {
        Write-Status "ERROR" "Dynamic capture file was not created"
        exit 1
    }

    Write-Status "OK" "`u{2705} All validation checks passed!"
    Log-ProcessEvent "TEST_SUCCESS" "All tests completed successfully"
    Log-ChildProcesses $PID "Before script exit"
    
} catch {
    Write-Status "ERROR" "Test failed: $($_.Exception.Message)"
    Log-ProcessEvent "TEST_ERROR" "Test failed: $($_.Exception.Message)"
    exit 1
} finally {
    Invoke-Cleanup
    
    # Show the process tracking log
    Write-Host ""
    Write-Host "=== Process Tracking Summary ==="
    if (Test-Path $script:ProcessLog) {
        Get-Content $script:ProcessLog | Write-Host
    } else {
        Write-Host "Process log not found"
    }
    
    Log-ProcessEvent "SCRIPT_END" "Script ending normally"
}
