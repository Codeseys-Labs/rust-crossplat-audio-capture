FROM ubuntu:22.04

# Set environment variables
ENV DEBIAN_FRONTEND=noninteractive
ENV XDG_RUNTIME_DIR=/run/user/0
ENV PIPEWIRE_RUNTIME_DIR=/run/user/0
ENV DISPLAY=:99
ENV LIBCLANG_PATH=/usr/lib/llvm-14/lib

# Install dependencies
RUN apt-get update && apt-get install -y \
    curl wget build-essential pkg-config \
    libclang-dev llvm-14 clang-14 \
    xvfb dbus \
    software-properties-common \
    && add-apt-repository ppa:pipewire-debian/pipewire-upstream \
    && apt-get update \
    && apt-get install -y pipewire=0.3.65-1 pipewire-bin=0.3.65-1 libpipewire-0.3-dev=0.3.65-1 \
    && mkdir -p /run/dbus \
    && mkdir -p /run/user/0 \
    && chmod 700 /run/user/0

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Create app directory
RUN mkdir -p /app
WORKDIR /app

# Copy project files
COPY . /app/

# Make test script executable
RUN chmod +x /docker/linux/test.sh

# Start Xvfb, D-Bus, Pipewire, and run test script
CMD /usr/bin/dbus-daemon --system --nofork --nopidfile & \
    Xvfb :99 -screen 0 1024x768x24 & \
    sleep 2 && \
    pipewire & \
    pipewire-pulse & \
    sleep 2 && \
    /docker/linux/test.sh 