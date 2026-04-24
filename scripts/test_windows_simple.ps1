#!/usr/bin/env pwsh

# Simple Windows WASAPI test script
# This script tests our enhanced Windows implementation

Write-Host "=== Windows WASAPI Enhanced Implementation Test ===" -ForegroundColor Green

# Build the project (Windows only)
Write-Host "Building Windows-only features..." -ForegroundColor Yellow
cargo build --features feat_windows --no-default-features --target x86_64-pc-windows-msvc

if ($LASTEXITCODE -ne 0) {
    Write-Host "Build failed! Trying alternative approach..." -ForegroundColor Red
    cargo build --features feat_windows
}

# Test Windows example
Write-Host "Testing Windows example..." -ForegroundColor Yellow
cargo run --example test_windows --features feat_windows -- --duration 3 --verbose

# Test device enumeration
Write-Host "Testing device enumeration..." -ForegroundColor Yellow
cargo run --example test_windows --features feat_windows -- --duration 1 --verbose --format f32le

# Test different audio formats
Write-Host "Testing different audio formats..." -ForegroundColor Yellow
$formats = @("s16le", "s32le", "f32le")
foreach ($format in $formats) {
    Write-Host "Testing format: $format" -ForegroundColor Cyan
    cargo run --example test_windows --features feat_windows -- --duration 2 --format $format --verbose
}

Write-Host "=== Test Complete ===" -ForegroundColor Green
Write-Host "Check output files in current directory" -ForegroundColor Cyan