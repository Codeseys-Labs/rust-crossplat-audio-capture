# Multi-stage Dockerfile for cross-platform Rust audio capture testing
# Based on cargo-xwin approach for robust cross-compilation

ARG RUST_VERSION=1.88.0

# =============================================================================
# Base stage with common dependencies
# =============================================================================
FROM rust:${RUST_VERSION} as base

ENV DEBIAN_FRONTEND=noninteractive
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV RUST_BACKTRACE=1

# Install common build tools and dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    pkg-config \
    cmake \
    ninja-build \
    clang \
    llvm \
    git \
    && rm -rf /var/lib/apt/lists/*

# Install Rust components for cross-compilation
RUN rustup component add llvm-tools-preview && \
    rustup target add \
        x86_64-unknown-linux-gnu \
        x86_64-pc-windows-msvc \
        aarch64-pc-windows-msvc \
        x86_64-apple-darwin \
        aarch64-apple-darwin

WORKDIR /app

# =============================================================================
# Linux testing stage
# =============================================================================
FROM base as linux-test

# Install Linux audio development libraries
RUN apt-get update && apt-get install -y \
    libpipewire-0.3-dev \
    libspa-0.2-dev \
    libpulse-dev \
    libasound2-dev \
    libclang-dev \
    pipewire \
    pipewire-audio-client-libraries \
    pulseaudio \
    alsa-utils \
    vlc \
    && rm -rf /var/lib/apt/lists/*

# Copy project files
COPY . .

# Build for Linux
RUN cargo build --target x86_64-unknown-linux-gnu --no-default-features --features feat_linux --examples

# =============================================================================
# Windows cross-compilation stage (using cargo-xwin approach)
# =============================================================================
FROM base as windows-cross

# Install wine for testing Windows binaries
RUN set -eux; \
    curl --fail https://dl.winehq.org/wine-builds/winehq.key | gpg --dearmor > /usr/share/keyrings/winehq.gpg; \
    echo "deb [signed-by=/usr/share/keyrings/winehq.gpg] https://dl.winehq.org/wine-builds/debian/ bookworm main" > /etc/apt/sources.list.d/winehq.list; \
    dpkg --add-architecture i386; \
    apt-get update && apt-get install --no-install-recommends -y winehq-staging; \
    apt-get remove -y --auto-remove; \
    rm -rf /var/lib/apt/lists/*;

# Install cargo-xwin
RUN cargo install --locked cargo-xwin

# Copy project files
COPY . .

# Build for Windows using cargo-xwin
RUN cargo xwin build --target x86_64-pc-windows-msvc --no-default-features --features feat_windows --examples

# =============================================================================
# macOS cross-compilation stage
# =============================================================================
FROM base as macos-cross

# Install macOS cross-compilation toolchain
RUN git clone https://github.com/tpoechtrager/osxcross.git /opt/osxcross && \
    cd /opt/osxcross && \
    wget -nc https://github.com/phracker/MacOSX-SDKs/releases/download/11.3/MacOSX11.3.sdk.tar.xz && \
    mv MacOSX11.3.sdk.tar.xz tarballs/ && \
    UNATTENDED=yes OSX_VERSION_MIN=10.7 ./build.sh

ENV PATH="/opt/osxcross/target/bin:$PATH"
ENV CC=o64-clang
ENV CXX=o64-clang++

# Copy project files
COPY . .

# Build for macOS (compilation check only)
RUN cargo build --target x86_64-apple-darwin --no-default-features --features feat_macos --examples

# =============================================================================
# Test runner stage
# =============================================================================
FROM linux-test as test-runner

# Install additional testing tools
RUN apt-get update && apt-get install -y \
    jq \
    xmlstarlet \
    && rm -rf /var/lib/apt/lists/*

# Copy test scripts
COPY scripts/ ./scripts/
COPY docker/unified/test-runner.sh ./test-runner.sh
RUN chmod +x ./test-runner.sh ./scripts/*.sh

# Create test results directory
RUN mkdir -p /test-results

# Default command runs all tests
CMD ["./test-runner.sh"]

# =============================================================================
# Development stage with all tools
# =============================================================================
FROM test-runner as dev

# Install development tools
RUN apt-get update && apt-get install -y \
    gdb \
    valgrind \
    strace \
    htop \
    vim \
    && rm -rf /var/lib/apt/lists/*

# Install cargo-xwin for Windows cross-compilation
RUN cargo install --locked cargo-xwin

# Install additional Rust tools
RUN cargo install \
    cargo-watch \
    cargo-audit \
    cargo-outdated

CMD ["/bin/bash"]
