# Base image with Rust
FROM rust:1.74-slim-bullseye as builder

# Install system dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    clang \
    llvm \
    && rm -rf /var/lib/apt/lists/*

# Create test user
RUN useradd -m -u 1000 testuser

# Create virtual audio device setup
COPY .docker/setup-audio-mac.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/setup-audio-mac.sh

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
ENTRYPOINT ["/usr/local/bin/setup-audio-mac.sh"]