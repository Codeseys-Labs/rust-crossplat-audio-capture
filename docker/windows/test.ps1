#!/usr/bin/env pwsh

# Stop on any error
$ErrorActionPreference = "Stop"

Write-Host "Starting Windows audio test environment..."

# Wait for audio service to be ready
Write-Host "Waiting for Audio service..."
while ((Get-Service Audiosrv).Status -ne 'Running') {
    Start-Sleep -Seconds 1
}
Write-Host "Audio service is running"

# Start VLC with test audio
Write-Host "Starting VLC with test audio..."
$vlcProcess = Start-Process -FilePath "C:\Program Files\VideoLAN\VLC\vlc.exe" `
    -ArgumentList "--loop", "--no-video", "C:\test_audio\test.mp3" `
    -PassThru

# Build the project
Write-Host "Building project..."
Set-Location C:\app
cargo build --release

# Run tests
Write-Host "Running tests..."

# Test system-wide capture
Write-Host "Testing system-wide capture..."
cargo run --release -- capture-system -o "C:\test_results\system_capture.wav" -d 5
if (-not (Test-Path "C:\test_results\system_capture.wav")) {
    throw "System capture test failed - output file not found"
}

# Test application-specific capture
Write-Host "Testing application-specific capture..."
cargo run --release -- list-applications
cargo run --release -- capture-application -n "VLC" -o "C:\test_results\vlc_capture.wav" -d 5
if (-not (Test-Path "C:\test_results\vlc_capture.wav")) {
    throw "Application capture test failed - output file not found"
}

# Test exclusive mode capture
Write-Host "Testing exclusive mode capture..."
cargo run --release -- capture-system -o "C:\test_results\exclusive_capture.wav" -d 5 --exclusive
if (-not (Test-Path "C:\test_results\exclusive_capture.wav")) {
    throw "Exclusive mode capture test failed - output file not found"
}

# Test concurrent capture
Write-Host "Testing concurrent capture..."
$systemCapture = Start-Process -FilePath "cargo" `
    -ArgumentList "run", "--release", "--", "capture-system", "-o", "C:\test_results\concurrent_system.wav", "-d", "10" `
    -PassThru
Start-Sleep -Seconds 2
$appCapture = Start-Process -FilePath "cargo" `
    -ArgumentList "run", "--release", "--", "capture-application", "-n", "VLC", "-o", "C:\test_results\concurrent_app.wav", "-d", "5" `
    -PassThru

$appCapture.WaitForExit()
$systemCapture.WaitForExit()

if (-not ((Test-Path "C:\test_results\concurrent_system.wav") -and (Test-Path "C:\test_results\concurrent_app.wav"))) {
    throw "Concurrent capture test failed - output files not found"
}

# Cleanup
Write-Host "Cleaning up..."
Stop-Process -Id $vlcProcess.Id -Force

Write-Host "Tests completed successfully!" 