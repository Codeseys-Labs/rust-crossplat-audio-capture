# Windows Testing Setup Script
# Sets up Windows environment for Rust audio capture testing

param(
    [string]$WorkspaceDir = "C:\workspace",
    [string]$ResultsDir = "C:\test-results\windows"
)

# Enable verbose output
$VerbosePreference = "Continue"

Write-Host "🪟 Setting up Windows Testing Environment..." -ForegroundColor Blue

# Create directories
New-Item -ItemType Directory -Force -Path $WorkspaceDir | Out-Null
New-Item -ItemType Directory -Force -Path $ResultsDir | Out-Null

# Set working directory
Set-Location $WorkspaceDir

# Function to log with timestamp
function Write-Log {
    param([string]$Message, [string]$Level = "INFO")
    $timestamp = Get-Date -Format "yyyy-MM-dd HH:mm:ss"
    $color = switch ($Level) {
        "ERROR" { "Red" }
        "WARNING" { "Yellow" }
        "SUCCESS" { "Green" }
        default { "White" }
    }
    Write-Host "[$timestamp] [$Level] $Message" -ForegroundColor $color
}

# Install Chocolatey if not present
if (!(Get-Command choco -ErrorAction SilentlyContinue)) {
    Write-Log "Installing Chocolatey..."
    Set-ExecutionPolicy Bypass -Scope Process -Force
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072
    iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))
    refreshenv
}

# Install required software
Write-Log "Installing required software..."

# Install Rust
if (!(Get-Command rustc -ErrorAction SilentlyContinue)) {
    Write-Log "Installing Rust..."
    choco install rust -y
    refreshenv
}

# Install VLC
if (!(Test-Path "C:\Program Files\VideoLAN\VLC\vlc.exe")) {
    Write-Log "Installing VLC..."
    choco install vlc -y
}

# Install Git
if (!(Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Log "Installing Git..."
    choco install git -y
    refreshenv
}

# Install Visual Studio Build Tools
if (!(Get-Command cl -ErrorAction SilentlyContinue)) {
    Write-Log "Installing Visual Studio Build Tools..."
    choco install visualstudio2022buildtools --package-parameters "--add Microsoft.VisualStudio.Workload.VCTools" -y
    refreshenv
}

# Install additional tools
Write-Log "Installing additional tools..."
choco install jq -y
choco install curl -y

# Refresh environment variables
refreshenv

# Verify installations
Write-Log "Verifying installations..."

$installations = @{
    "Rust" = { rustc --version }
    "Cargo" = { cargo --version }
    "VLC" = { Test-Path "C:\Program Files\VideoLAN\VLC\vlc.exe" }
    "Git" = { git --version }
}

foreach ($tool in $installations.Keys) {
    try {
        $result = & $installations[$tool]
        if ($result) {
            Write-Log "$tool is installed: $result" "SUCCESS"
        } else {
            Write-Log "$tool installation verification failed" "WARNING"
        }
    } catch {
        Write-Log "$tool is not available: $($_.Exception.Message)" "ERROR"
    }
}

# Set up Rust environment
Write-Log "Setting up Rust environment..."
rustup component add clippy rustfmt
cargo install cargo-watch

# Create test audio files
Write-Log "Creating test audio files..."
$testAudioDir = "C:\test-audio"
New-Item -ItemType Directory -Force -Path $testAudioDir | Out-Null

# Copy project files from host
if (Test-Path "\\host.lan\Data") {
    Write-Log "Copying project files from host..."
    Copy-Item "\\host.lan\Data\*" -Destination $WorkspaceDir -Recurse -Force
} else {
    Write-Log "Host data not available, project files need to be copied manually" "WARNING"
}

# Test audio system
Write-Log "Testing Windows audio system..."
$audioDevices = Get-WmiObject -Class Win32_SoundDevice
if ($audioDevices) {
    Write-Log "Found $($audioDevices.Count) audio device(s)" "SUCCESS"
    foreach ($device in $audioDevices) {
        Write-Log "Audio Device: $($device.Name)"
    }
} else {
    Write-Log "No audio devices found" "WARNING"
}

# Create test script
$testScript = @"
# Windows Audio Capture Test Script
param([string]`$ResultsDir = "C:\test-results\windows")

`$timestamp = Get-Date -Format "yyyyMMdd_HHmmss"
`$logFile = "`$ResultsDir\windows_test_`$timestamp.log"

function Write-TestLog {
    param([string]`$Message)
    `$timestampedMessage = "[(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')] `$Message"
    Write-Host `$timestampedMessage
    Add-Content -Path `$logFile -Value `$timestampedMessage
}

Write-TestLog "🪟 Starting Windows Audio Capture Testing..."

# Test library build
Write-TestLog "Building Rust audio capture library..."
if (cargo build --no-default-features --features feat_windows) {
    Write-TestLog "✅ Library build successful"
} else {
    Write-TestLog "❌ Library build failed"
}

# Test dynamic_vlc example build
if (Test-Path "examples\dynamic_vlc.rs") {
    Write-TestLog "Building dynamic_vlc example..."
    if (cargo build --example dynamic_vlc --no-default-features --features feat_windows) {
        Write-TestLog "✅ dynamic_vlc example build successful"
    } else {
        Write-TestLog "❌ dynamic_vlc example build failed"
    }
} else {
    Write-TestLog "⚠️ dynamic_vlc example not found"
}

# Test VLC
Write-TestLog "Testing VLC..."
`$vlcPath = "C:\Program Files\VideoLAN\VLC\vlc.exe"
if (Test-Path `$vlcPath) {
    Write-TestLog "✅ VLC is available"
    
    # Test VLC version
    `$vlcVersion = & "`$vlcPath" --version 2>&1 | Select-Object -First 1
    Write-TestLog "VLC Version: `$vlcVersion"
    
    # Test VLC audio playback (if test audio exists)
    if (Test-Path "C:\test-audio\test-tone-440hz.wav") {
        Write-TestLog "Testing VLC audio playback..."
        Start-Process -FilePath "`$vlcPath" -ArgumentList "--intf", "dummy", "--play-and-exit", "C:\test-audio\test-tone-440hz.wav" -Wait
        Write-TestLog "✅ VLC audio playback test completed"
    }
} else {
    Write-TestLog "❌ VLC not found"
}

# Test audio capture
Write-TestLog "Testing audio capture functionality..."
if (Test-Path "target\debug\examples\windows_application_capture.exe") {
    Write-TestLog "Running windows_application_capture example..."
    `$captureOutput = & "target\debug\examples\windows_application_capture.exe" 2>&1
    Write-TestLog "Capture output: `$captureOutput"
} else {
    Write-TestLog "⚠️ windows_application_capture example not built"
}

Write-TestLog "🎉 Windows testing completed!"
"@

$testScript | Out-File -FilePath "C:\test-windows.ps1" -Encoding UTF8

Write-Log "Windows testing environment setup completed!" "SUCCESS"
Write-Log "To run tests, execute: PowerShell -ExecutionPolicy Bypass -File C:\test-windows.ps1" "INFO"
Write-Log "Access the Windows environment via web browser at http://localhost:8006" "INFO"
