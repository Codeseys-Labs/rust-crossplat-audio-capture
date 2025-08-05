#!/usr/bin/env pwsh

# Windows WASAPI Compilation Test Script
# Tests our enhanced Windows implementation compilation

Write-Host "=== Windows WASAPI Enhanced Implementation Compilation Test ===" -ForegroundColor Green
Write-Host "Testing from: $PWD" -ForegroundColor Cyan

# Check if we're in the right directory
if (-not (Test-Path "Cargo.toml")) {
    Write-Host "Error: Cargo.toml not found. Please run from project root." -ForegroundColor Red
    exit 1
}

# Display system info
Write-Host "`n=== System Information ===" -ForegroundColor Yellow
Write-Host "OS: $([System.Environment]::OSVersion.VersionString)"
Write-Host "Platform: $([System.Environment]::OSVersion.Platform)"
Write-Host "Architecture: $([System.Environment]::Is64BitOperatingSystem)"

# Check Rust installation
Write-Host "`n=== Checking Rust Installation ===" -ForegroundColor Yellow
try {
    $rustVersion = cargo --version
    Write-Host "Rust found: $rustVersion" -ForegroundColor Green
} catch {
    Write-Host "Rust not found. Installing Rust..." -ForegroundColor Yellow
    
    # Download and install Rust
    Invoke-WebRequest -Uri "https://win.rustup.rs" -OutFile "rustup-init.exe"
    Start-Process -FilePath ".\rustup-init.exe" -ArgumentList "-y" -Wait
    Remove-Item "rustup-init.exe"
    
    # Refresh environment variables
    $env:PATH = [System.Environment]::GetEnvironmentVariable("PATH", "Machine") + ";" + [System.Environment]::GetEnvironmentVariable("PATH", "User")
    
    $rustVersion = cargo --version
    Write-Host "Rust installed: $rustVersion" -ForegroundColor Green
}

# Clean previous builds
Write-Host "`n=== Cleaning Previous Builds ===" -ForegroundColor Yellow
if (Test-Path "target") {
    Remove-Item -Recurse -Force "target"
    Write-Host "Cleaned target directory" -ForegroundColor Green
}

# Test Windows-only compilation
Write-Host "`n=== Testing Windows-Only Compilation ===" -ForegroundColor Yellow
Write-Host "Building with Windows features only..." -ForegroundColor Cyan

try {
    # Build Windows-only features
    $buildResult = cargo build --no-default-features --features feat_windows --target x86_64-pc-windows-msvc 2>&1
    
    if ($LASTEXITCODE -eq 0) {
        Write-Host "✅ Windows-only build SUCCESSFUL!" -ForegroundColor Green
    } else {
        Write-Host "❌ Windows-only build failed. Error output:" -ForegroundColor Red
        Write-Host $buildResult -ForegroundColor Red
        
        # Try alternative build approach
        Write-Host "`nTrying alternative build approach..." -ForegroundColor Yellow
        $altBuildResult = cargo build --features feat_windows 2>&1
        
        if ($LASTEXITCODE -eq 0) {
            Write-Host "✅ Alternative Windows build SUCCESSFUL!" -ForegroundColor Green
        } else {
            Write-Host "❌ Alternative build also failed:" -ForegroundColor Red
            Write-Host $altBuildResult -ForegroundColor Red
        }
    }
} catch {
    Write-Host "❌ Build process failed with exception: $($_.Exception.Message)" -ForegroundColor Red
}

# Test examples compilation
Write-Host "`n=== Testing Windows Examples Compilation ===" -ForegroundColor Yellow

$examples = @("windows_device_test", "test_windows", "windows_apis")

foreach ($example in $examples) {
    Write-Host "Building example: $example" -ForegroundColor Cyan
    try {
        $exampleResult = cargo build --example $example --features feat_windows 2>&1
        
        if ($LASTEXITCODE -eq 0) {
            Write-Host "✅ Example $example built successfully!" -ForegroundColor Green
        } else {
            Write-Host "❌ Example $example failed to build:" -ForegroundColor Red
            Write-Host $exampleResult -ForegroundColor Red
        }
    } catch {
        Write-Host "❌ Exception building $example: $($_.Exception.Message)" -ForegroundColor Red
    }
}

# Test running the device test example
Write-Host "`n=== Testing Windows Device Test Example ===" -ForegroundColor Yellow
try {
    Write-Host "Running windows_device_test example..." -ForegroundColor Cyan
    $testResult = cargo run --example windows_device_test --features feat_windows 2>&1
    
    if ($LASTEXITCODE -eq 0) {
        Write-Host "✅ Device test ran successfully!" -ForegroundColor Green
        Write-Host "Output:" -ForegroundColor Cyan
        Write-Host $testResult
    } else {
        Write-Host "❌ Device test failed:" -ForegroundColor Red
        Write-Host $testResult -ForegroundColor Red
    }
} catch {
    Write-Host "❌ Exception running device test: $($_.Exception.Message)" -ForegroundColor Red
}

Write-Host "`n=== Compilation Test Complete ===" -ForegroundColor Green
Write-Host "Check the results above to verify the enhanced WASAPI implementation compiles and runs correctly." -ForegroundColor Cyan