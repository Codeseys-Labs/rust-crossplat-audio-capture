# Base image
FROM ubuntu:22.04

# Set DEBIAN_FRONTEND to noninteractive to avoid prompts during apt-get install
ENV DEBIAN_FRONTEND=noninteractive

# Install essential build tools, curl, pkg-config, and audio development libraries
# build-essential: Common build tools (make, gcc, g++, etc.)
# curl: Tool for transferring data with URLs (used for rustup)
# pkg-config: Helper tool used when compiling applications and libraries
# libpipewire-0.3-dev: Development files for PipeWire
# libpulse-dev: Development files for PulseAudio
# libasound2-dev: Development files for ALSA (Advanced Linux Sound Architecture)
# libclang-dev: Development files for libclang, often needed for Rust crates with C bindings
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    pkg-config \
    libpipewire-0.3-dev \
    libpulse-dev \
    libasound2-dev \
    libclang-dev \
    && rm -rf /var/lib/apt/lists/*

# Install Rust via rustup
# -y: Disable confirmation prompts
# --default-toolchain stable: Install the stable toolchain and set it as default
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable

# Add Cargo to PATH
ENV PATH="/root/.cargo/bin:${PATH}"

# Set RUST_BACKTRACE for better debugging of test failures
ENV RUST_BACKTRACE=1

# Set working directory
WORKDIR /app

# Copy the entire project context to the working directory
COPY . .

# Build the project with all features and all targets
# --all-features: Activate all features
# --all-targets: Build for all targets (binaries, libraries, examples, tests, benchmarks)
RUN cargo build --all-features --all-targets

# Run tests with all features and all targets
RUN cargo test --all-features --all-targets