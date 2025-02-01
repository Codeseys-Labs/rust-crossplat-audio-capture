# Windows container with Rust
FROM mcr.microsoft.com/windows/servercore:ltsc2019 as builder

# Install Chocolatey
RUN powershell -Command \
    Set-ExecutionPolicy Bypass -Scope Process -Force; \
    [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.ServicePointManager]::SecurityProtocol -bor 3072; \
    iex ((New-Object System.Net.WebClient).DownloadString('https://chocolatey.org/install.ps1'))

# Install Rust and build tools
RUN choco install -y rust-ms visualstudio2019buildtools

# Create test user and directory
RUN net user testuser /add && \
    mkdir C:\app && \
    icacls C:\app /grant testuser:(OI)(CI)F

# Copy audio setup script
COPY .docker/setup-audio-win.ps1 C:\Windows\System32\
RUN powershell -Command \
    $acl = Get-Acl C:\Windows\System32\setup-audio-win.ps1; \
    $rule = New-Object System.Security.AccessControl.FileSystemAccessRule('testuser','ReadAndExecute','Allow'); \
    $acl.SetAccessRule($rule); \
    Set-Acl C:\Windows\System32\setup-audio-win.ps1

USER testuser
WORKDIR C:\app

# Pre-build dependencies
COPY --chown=testuser Cargo.toml Cargo.lock ./
RUN mkdir src && echo fn main() {} > src\main.rs && \
    cargo build --release && \
    rd /s /q src

# Copy source code
COPY --chown=testuser . .

# Entry point script
ENTRYPOINT ["powershell", "-File", "C:\\Windows\\System32\\setup-audio-win.ps1"]