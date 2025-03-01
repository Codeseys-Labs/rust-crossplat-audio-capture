FROM mcr.microsoft.com/windows:ltsc2022

# Set environment variables
ENV VERSION=11
ENV RAM_SIZE=4G
ENV CPU_CORES=2
ENV DISK_SIZE=64G
ENV USERNAME=TestUser
ENV PASSWORD=TestPass123
ENV AUDIO_ALLOW_INSECURE=1
ENV WASAPI_ALLOW_EXCLUSIVE=1
ENV AUDIO_DEVICE_PERIOD=50
ENV AUDIO_DEVICE_BUFFER_FRAMES=1024
ENV RUST_BACKTRACE=1
ENV RUST_LOG=debug
ENV PATH="C:\Program Files\PowerShell\7;C:\Program Files\VideoLAN\VLC;C:\Windows\system32;C:\Windows;C:\Windows\System32\Wbem;${PATH}"

# Install PowerShell and build tools using Windows commands
SHELL ["cmd", "/S", "/C"]

# Download and install PowerShell
RUN powershell -Command \
    "Set-ExecutionPolicy Bypass -Scope Process -Force; \
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; \
    Invoke-WebRequest -Uri https://github.com/PowerShell/PowerShell/releases/download/v7.4.1/PowerShell-7.4.1-win-x64.msi -OutFile PowerShell.msi; \
    Start-Process msiexec.exe -Wait -ArgumentList '/i PowerShell.msi /quiet'; \
    Remove-Item PowerShell.msi"

# Switch to PowerShell 7 for remaining commands
SHELL ["pwsh", "-Command"]

# Install Chocolatey and other dependencies
RUN Set-ExecutionPolicy Bypass -Scope Process -Force; \
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; \
    iex ((New-Object System.Net.WebClient).DownloadString('https://chocolatey.org/install.ps1')); \
    choco install -y --no-progress vlc visualstudio2022buildtools visualstudio2022-workload-vctools windows-sdk-10.0

# Install Rust
RUN Invoke-WebRequest -Uri https://win.rustup.rs/x86_64 -OutFile rustup-init.exe; \
    Start-Process -Wait -FilePath .\rustup-init.exe -ArgumentList '-y', '--default-toolchain', 'stable', '--no-modify-path'; \
    Remove-Item -Path rustup-init.exe; \
    $env:Path = [System.Environment]::GetEnvironmentVariable('Path', 'Machine')

# Create test directories
RUN New-Item -ItemType Directory -Path C:\app, C:\test_audio, C:\test_results -Force

# Copy test script
COPY docker/windows/test.ps1 C:\test.ps1

WORKDIR C:\app

# Health check
HEALTHCHECK --interval=5s --timeout=3s --start-period=2s --retries=3 \
    CMD pwsh -Command "Get-Service Audiosrv | Where-Object {$_.Status -eq 'Running'}"

# Volume for test results
VOLUME C:\test_results

# Start Windows and run tests
CMD ["pwsh", "-File", "C:\\test.ps1"] 