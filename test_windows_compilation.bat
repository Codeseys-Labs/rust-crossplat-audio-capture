@echo off
echo === Windows WASAPI Enhanced Implementation Compilation Test ===
echo Testing from: %CD%

REM Check if we're in the right directory
if not exist "Cargo.toml" (
    echo Error: Cargo.toml not found. Please run from project root.
    pause
    exit /b 1
)

echo.
echo === System Information ===
echo OS: %OS%
echo Processor: %PROCESSOR_ARCHITECTURE%
echo Computer: %COMPUTERNAME%

echo.
echo === Checking Rust Installation ===
cargo --version >nul 2>&1
if %errorlevel% equ 0 (
    for /f %%i in ('cargo --version') do echo Rust found: %%i
) else (
    echo Rust not found. Please install Rust from https://rustup.rs/
    echo Download rustup-init.exe and run it to install Rust
    pause
    exit /b 1
)

echo.
echo === Cleaning Previous Builds ===
if exist "target" (
    rmdir /s /q "target"
    echo Cleaned target directory
)

echo.
echo === Testing Windows-Only Compilation ===
echo Building with Windows features only...

REM Build Windows-only features
cargo build --no-default-features --features feat_windows --target x86_64-pc-windows-msvc
if %errorlevel% equ 0 (
    echo ✅ Windows-only build SUCCESSFUL!
) else (
    echo ❌ Windows-only build failed. Trying alternative approach...
    cargo build --features feat_windows
    if %errorlevel% equ 0 (
        echo ✅ Alternative Windows build SUCCESSFUL!
    ) else (
        echo ❌ Alternative build also failed
    )
)

echo.
echo === Testing Windows Examples Compilation ===

REM Test examples compilation
set examples=windows_device_test test_windows windows_apis

for %%e in (%examples%) do (
    echo Building example: %%e
    cargo build --example %%e --features feat_windows
    if %errorlevel% equ 0 (
        echo ✅ Example %%e built successfully!
    ) else (
        echo ❌ Example %%e failed to build
    )
)

echo.
echo === Testing Windows Device Test Example ===
echo Running windows_device_test example...
cargo run --example windows_device_test --features feat_windows
if %errorlevel% equ 0 (
    echo ✅ Device test ran successfully!
) else (
    echo ❌ Device test failed
)

echo.
echo === Compilation Test Complete ===
echo Check the results above to verify the enhanced WASAPI implementation compiles and runs correctly.
pause