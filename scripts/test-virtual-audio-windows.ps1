# Windows Virtual Audio Driver Testing Script
# This script helps diagnose and test virtual audio driver installation and functionality

param(
    [string]$DriverPath = "",
    [string]$TestDuration = "10",
    [switch]$Verbose = $false,
    [switch]$SkipDriverInstall = $false
)

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

function Test-AudioDevices {
    param([string]$Phase)
    
    Write-Status "INFO" "=== AUDIO DEVICE ANALYSIS - $Phase ==="
    
    # Check Windows Audio Service
    Write-Status "INFO" "Checking Windows Audio Service..."
    $audioService = Get-Service -Name "AudioSrv" -ErrorAction SilentlyContinue
    if ($audioService) {
        Write-Status "INFO" "Audio Service Status: $($audioService.Status)"
        if ($audioService.Status -ne "Running") {
            Write-Status "WARN" "Audio service is not running"
        }
    } else {
        Write-Status "ERROR" "Audio service not found"
    }
    
    # List audio devices via WMI
    Write-Status "INFO" "Enumerating audio devices via WMI..."
    try {
        $audioDevices = Get-WmiObject -Class Win32_SoundDevice -ErrorAction Stop
        if ($audioDevices) {
            Write-Status "OK" "Found $($audioDevices.Count) audio device(s)"
            $audioDevices | ForEach-Object {
                Write-Status "INFO" "  Device: $($_.Name) - Status: $($_.Status)"
            }
        } else {
            Write-Status "WARN" "No audio devices found via WMI"
        }
    } catch {
        Write-Status "ERROR" "Failed to enumerate audio devices via WMI: $_"
    }
    
    # Check PnP devices
    Write-Status "INFO" "Checking PnP audio devices..."
    try {
        $pnpAudioDevices = Get-PnpDevice | Where-Object { 
            $_.Class -eq "MEDIA" -or 
            $_.Class -eq "AudioEndpoint" -or 
            $_.FriendlyName -like "*Audio*" -or 
            $_.FriendlyName -like "*Sound*" -or
            $_.FriendlyName -like "*Virtual*"
        }
        
        if ($pnpAudioDevices) {
            Write-Status "OK" "Found $($pnpAudioDevices.Count) PnP audio device(s)"
            $pnpAudioDevices | ForEach-Object {
                $status = if ($_.Status -eq "OK") { "OK" } else { "WARN" }
                Write-Status $status "  $($_.FriendlyName) - $($_.Status)"
            }
        } else {
            Write-Status "WARN" "No PnP audio devices found"
        }
    } catch {
        Write-Status "ERROR" "Failed to check PnP devices: $_"
    }
    
    # Check for virtual audio devices specifically
    Write-Status "INFO" "Looking for virtual audio devices..."
    $virtualDevices = Get-PnpDevice | Where-Object { 
        $_.FriendlyName -like "*Virtual*" -and 
        ($_.Class -eq "MEDIA" -or $_.Class -eq "AudioEndpoint")
    }
    
    if ($virtualDevices) {
        Write-Status "OK" "Found virtual audio devices:"
        $virtualDevices | ForEach-Object {
            Write-Status "INFO" "  $($_.FriendlyName) - $($_.Status)"
        }
    } else {
        Write-Status "INFO" "No virtual audio devices detected"
    }
}

function Install-VirtualAudioDriver {
    param([string]$DriverPath)
    
    Write-Status "INFO" "=== VIRTUAL AUDIO DRIVER INSTALLATION ==="
    
    if ($SkipDriverInstall) {
        Write-Status "INFO" "Skipping driver installation (--SkipDriverInstall specified)"
        return
    }
    
    # Try to find driver files
    $infFiles = @()
    
    if ($DriverPath -and (Test-Path $DriverPath)) {
        Write-Status "INFO" "Using specified driver path: $DriverPath"
        if (Test-Path $DriverPath -PathType Container) {
            $infFiles = Get-ChildItem -Path $DriverPath -Recurse -Filter "*.inf"
        } elseif ($DriverPath -like "*.inf") {
            $infFiles = @(Get-Item $DriverPath)
        }
    } else {
        Write-Status "INFO" "Searching for driver files in current directory..."
        $infFiles = Get-ChildItem -Recurse -Filter "*.inf" | Where-Object { 
            $_.Name -like "*Audio*" -or 
            $_.Name -like "*Virtual*" -or
            $_.Directory.Name -like "*Audio*" -or
            $_.Directory.Name -like "*Virtual*"
        }
    }
    
    if ($infFiles.Count -eq 0) {
        Write-Status "WARN" "No suitable INF files found for virtual audio driver"
        Write-Status "INFO" "Available INF files:"
        Get-ChildItem -Recurse -Filter "*.inf" | ForEach-Object {
            Write-Status "INFO" "  $($_.FullName)"
        }
        return
    }
    
    Write-Status "OK" "Found $($infFiles.Count) potential driver file(s):"
    $infFiles | ForEach-Object {
        Write-Status "INFO" "  $($_.FullName)"
    }
    
    # Try to install the first suitable driver
    $infFile = $infFiles[0].FullName
    Write-Status "INFO" "Attempting to install driver: $infFile"
    
    try {
        # Method 1: PnPUtil (modern approach)
        Write-Status "INFO" "Trying PnPUtil installation..."
        $pnpResult = & pnputil.exe /add-driver $infFile /install 2>&1
        
        if ($LASTEXITCODE -eq 0) {
            Write-Status "OK" "Driver installed successfully with PnPUtil"
            Write-Status "INFO" "PnPUtil output: $pnpResult"
        } else {
            Write-Status "WARN" "PnPUtil failed with exit code: $LASTEXITCODE"
            Write-Status "INFO" "PnPUtil output: $pnpResult"
            
            # Method 2: Try alternative approach
            Write-Status "INFO" "Trying alternative installation method..."
            try {
                $installResult = & rundll32.exe setupapi,InstallHinfSection DefaultInstall 132 $infFile 2>&1
                Write-Status "INFO" "Alternative installation result: $installResult"
            } catch {
                Write-Status "ERROR" "Alternative installation failed: $_"
            }
        }
    } catch {
        Write-Status "ERROR" "Driver installation failed: $_"
    }
    
    # Restart audio service to detect new devices
    Write-Status "INFO" "Restarting Windows Audio Service to detect new devices..."
    try {
        Restart-Service -Name "AudioSrv" -Force
        Start-Sleep -Seconds 3
        Write-Status "OK" "Audio service restarted"
    } catch {
        Write-Status "WARN" "Could not restart audio service: $_"
    }
}

function Test-VLCWithVirtualAudio {
    param([string]$Duration)
    
    Write-Status "INFO" "=== VLC VIRTUAL AUDIO TEST ==="
    
    # Find VLC
    $vlcPaths = @(
        "${env:ProgramFiles}\VideoLAN\VLC\vlc.exe",
        "${env:ProgramFiles(x86)}\VideoLAN\VLC\vlc.exe"
    )
    
    $vlcPath = $null
    foreach ($path in $vlcPaths) {
        if (Test-Path $path) {
            $vlcPath = $path
            break
        }
    }
    
    if (-not $vlcPath) {
        Write-Status "WARN" "VLC not found in standard locations, searching..."
        $vlcSearch = Get-ChildItem -Path "${env:ProgramFiles}*" -Recurse -Filter "vlc.exe" -ErrorAction SilentlyContinue
        if ($vlcSearch) {
            $vlcPath = $vlcSearch[0].FullName
        }
    }
    
    if (-not $vlcPath) {
        Write-Status "ERROR" "VLC Media Player not found"
        return
    }
    
    Write-Status "OK" "Found VLC at: $vlcPath"
    
    # Test VLC audio output
    Write-Status "INFO" "Testing VLC audio output with virtual devices..."
    
    $testUrl = "https://www.soundjay.com/misc/sounds/bell-ringing-05.wav"
    $logFile = "vlc_virtual_test.log"
    
    $vlcArgs = @(
        "--intf", "dummy",
        "--verbose", "2",
        "--extraintf", "logger",
        "--logfile", $logFile,
        $testUrl,
        "--play-and-exit",
        "--run-time", $Duration
    )
    
    Write-Status "INFO" "Starting VLC with virtual audio test..."
    Write-Status "INFO" "Command: $vlcPath $($vlcArgs -join ' ')"
    
    try {
        $vlcProcess = Start-Process -FilePath $vlcPath -ArgumentList $vlcArgs -PassThru -NoNewWindow
        Write-Status "INFO" "VLC started with PID: $($vlcProcess.Id)"
        
        # Wait for VLC to complete
        $timeout = [int]$Duration + 30
        $completed = $vlcProcess.WaitForExit($timeout * 1000)
        
        if ($completed) {
            Write-Status "OK" "VLC completed with exit code: $($vlcProcess.ExitCode)"
        } else {
            Write-Status "WARN" "VLC did not complete within timeout, terminating..."
            $vlcProcess.Kill()
        }
    } catch {
        Write-Status "ERROR" "Failed to run VLC test: $_"
    }
    
    # Analyze VLC log
    if (Test-Path $logFile) {
        Write-Status "INFO" "Analyzing VLC log file..."
        $logContent = Get-Content $logFile
        
        # Look for audio device information
        $audioLines = $logContent | Select-String -Pattern "audio|device|mmdevice|wasapi" -CaseSensitive:$false
        
        if ($audioLines) {
            Write-Status "INFO" "Audio-related log entries:"
            $audioLines | Select-Object -First 20 | ForEach-Object {
                Write-Status "INFO" "  $_"
            }
        }
        
        # Look for errors
        $errorLines = $logContent | Select-String -Pattern "error|failed|cannot" -CaseSensitive:$false
        if ($errorLines) {
            Write-Status "WARN" "Error entries found in VLC log:"
            $errorLines | Select-Object -First 10 | ForEach-Object {
                Write-Status "WARN" "  $_"
            }
        }
    } else {
        Write-Status "WARN" "VLC log file not found: $logFile"
    }
}

# Main execution
Write-Status "INFO" "Starting Windows Virtual Audio Driver Test"
Write-Status "INFO" "Parameters: Duration=$TestDuration, Verbose=$Verbose, SkipDriverInstall=$SkipDriverInstall"

# Phase 1: Initial audio system state
Test-AudioDevices "BEFORE"

# Phase 2: Install virtual audio driver
Install-VirtualAudioDriver $DriverPath

# Phase 3: Check audio system after driver installation
Test-AudioDevices "AFTER"

# Phase 4: Test VLC with virtual audio
Test-VLCWithVirtualAudio $TestDuration

Write-Status "OK" "Virtual audio driver test completed!"
