#!/usr/bin/env pwsh

# Windows setup script for Rust audio capture testing
# This runs after Windows installation is complete

Write-Host "Starting Rust development environment setup..."

# Set execution policy
Set-ExecutionPolicy Bypass -Scope Process -Force

# Install Chocolatey
Write-Host "Installing Chocolatey..."
[System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072
iex ((New-Object System.Net.WebClient).DownloadString('https://chocolatey.org/install.ps1'))

# Install dependencies
Write-Host "Installing build tools and VLC..."
choco install -y --no-progress vlc git visualstudio2022buildtools visualstudio2022-workload-vctools windows-sdk-10.0

# Install Rust
Write-Host "Installing Rust..."
Invoke-WebRequest -Uri https://win.rustup.rs/x86_64 -OutFile rustup-init.exe
Start-Process -Wait -FilePath .\rustup-init.exe -ArgumentList '-y', '--default-toolchain', 'stable'
Remove-Item -Path rustup-init.exe

# Add Rust to PATH for current session
$env:Path += ";$env:USERPROFILE\.cargo\bin"

# Create directories
Write-Host "Creating directories..."
New-Item -ItemType Directory -Path C:\app, C:\test_audio, C:\test_results -Force

# Copy project files
Write-Host "Copying project files..."
Copy-Item -Path "\\host.lan\Data\project\*" -Destination C:\app -Recurse -Force

# Copy test audio file if it exists
if (Test-Path "\\host.lan\Data\project\test_audio.mp3") {
    Copy-Item -Path "\\host.lan\Data\project\test_audio.mp3" -Destination C:\test_audio\test.mp3
}

# Copy test script
Copy-Item -Path "\\host.lan\Data\test.ps1" -Destination C:\test.ps1

Write-Host "Setup complete! Ready to run audio tests."
Write-Host "To run tests, execute: C:\test.ps1"