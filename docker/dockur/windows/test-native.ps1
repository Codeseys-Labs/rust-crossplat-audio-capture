# RSAC Native Windows Test Runner
# Run this inside the Windows VM after the OEM setup has completed.
#
# Usage (from PowerShell):
#   Z:\docker\dockur\windows\test-native.ps1

$ErrorActionPreference = "Stop"

Write-Host "=== RSAC Windows Native Test Runner ===" -ForegroundColor Cyan
Write-Host ""

# Ensure shared drive is mapped
if (-not (Test-Path "Z:\")) {
    Write-Host "Mapping shared drive..." -ForegroundColor Yellow
    net use Z: \\host.lan\Data
}

# Navigate to project root
Set-Location "Z:\"

# Verify Rust installation
Write-Host "--- Rust Version ---" -ForegroundColor Green
try {
    rustup show
    cargo --version
} catch {
    Write-Host "ERROR: Rust not found. Run the OEM install first or install manually:" -ForegroundColor Red
    Write-Host "  choco install -y rustup.install" -ForegroundColor Yellow
    exit 1
}

# Check Windows Audio Service
Write-Host "`n--- Audio Service Status ---" -ForegroundColor Green
$audioSvc = Get-Service -Name "Audiosrv" -ErrorAction SilentlyContinue
if ($audioSvc) {
    $audioSvc | Format-Table Status, Name, DisplayName -AutoSize
    if ($audioSvc.Status -ne "Running") {
        Write-Host "Starting Windows Audio service..." -ForegroundColor Yellow
        Start-Service -Name "Audiosrv" -ErrorAction SilentlyContinue
    }
} else {
    Write-Host "WARNING: Windows Audio service not found" -ForegroundColor Yellow
}

# List audio devices (verifies WASAPI device availability)
Write-Host "`n--- Audio Devices ---" -ForegroundColor Green
Get-WmiObject Win32_SoundDevice | Format-Table Name, Status -AutoSize

# Create test results directory
$resultsDir = "Z:\test-results"
New-Item -ItemType Directory -Path $resultsDir -Force | Out-Null

# Run compilation check
Write-Host "`n--- Cargo Check (Windows features) ---" -ForegroundColor Green
cargo check --features feat_windows 2>&1 | Tee-Object -FilePath "$resultsDir\cargo-check-windows.log"
$checkExit = $LASTEXITCODE

if ($checkExit -ne 0) {
    Write-Host "`nERROR: cargo check failed (exit code $checkExit)" -ForegroundColor Red
    Write-Host "See $resultsDir\cargo-check-windows.log for details" -ForegroundColor Yellow
    exit $checkExit
}

Write-Host "`nCompilation check passed!" -ForegroundColor Green

# Run tests (single-threaded to avoid audio device contention)
Write-Host "`n--- Cargo Test (Windows features) ---" -ForegroundColor Green
cargo test --features feat_windows -- --test-threads=1 2>&1 | Tee-Object -FilePath "$resultsDir\cargo-test-windows.log"
$testExit = $LASTEXITCODE

Write-Host ""
Write-Host "=== Tests Complete ===" -ForegroundColor Cyan
Write-Host "Results saved to $resultsDir\"
Write-Host "  - cargo-check-windows.log"
Write-Host "  - cargo-test-windows.log"

if ($testExit -ne 0) {
    Write-Host "`nSome tests failed (exit code $testExit)" -ForegroundColor Yellow
}

exit $testExit
