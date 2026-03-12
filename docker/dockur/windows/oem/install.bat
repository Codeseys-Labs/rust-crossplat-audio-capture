@echo off
echo === RSAC Windows Dev Environment Setup ===
echo Installing development tools...

REM Install Chocolatey package manager
powershell -Command "Set-ExecutionPolicy Bypass -Scope Process -Force; [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; iex ((New-Object System.Net.WebClient).DownloadString('https://community.chocolatey.org/install.ps1'))"

REM Refresh PATH so choco is available
call refreshenv

REM Install Git
choco install -y --no-progress git

REM Install Rust via rustup
choco install -y --no-progress rustup.install

REM Install Visual Studio 2022 Build Tools with C++ workload
choco install -y --no-progress visualstudio2022buildtools
choco install -y --no-progress visualstudio2022-workload-vctools

REM Map shared drive (project root from host)
net use Z: \\host.lan\Data /persistent:yes

echo.
echo === Setup Complete ===
echo.
echo Project available at Z:\
echo Run: cd /d Z:\ ^&^& cargo test --features feat_windows
echo.
