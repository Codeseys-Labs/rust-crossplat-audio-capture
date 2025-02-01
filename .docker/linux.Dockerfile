# Base image with Rust
FROM rust:1.74-slim-bullseye as builder

# Install system dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libasound2-dev \
    libpulse-dev \
    pipewire \
    pipewire-pulse \
    pipewire-audio \
    pulseaudio \
    pulseaudio-utils \
    dbus \
    && rm -rf /var/lib/apt/lists/*

# Create test user
RUN useradd -m -u 1000 testuser

# Set up audio configuration
RUN mkdir -p /home/testuser/.config/pulse && \
    echo "exit-idle-time = -1" > /home/testuser/.config/pulse/daemon.conf && \
    echo "anonymous-enable = yes" >> /home/testuser/.config/pulse/daemon.conf && \
    chown -R testuser:testuser /home/testuser/.config

# Create virtual audio device setup script
COPY .docker/setup-audio.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/setup-audio.sh

# Switch to test user
USER testuser
WORKDIR /home/testuser/app

# Pre-build dependencies
COPY --chown=testuser:testuser Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy source code
COPY --chown=testuser:testuser . .

# Entry point script
ENTRYPOINT ["/usr/local/bin/setup-audio.sh"]