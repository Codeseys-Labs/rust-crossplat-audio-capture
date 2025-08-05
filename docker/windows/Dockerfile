# Use dockurr/windows as base for Windows in Docker
FROM dockurr/windows

# Set environment variables for Windows version and resources
ENV VERSION=11
ENV RAM_SIZE=4G
ENV CPU_CORES=2
ENV DISK_SIZE=64G
ENV USERNAME=RustDev
ENV PASSWORD=AudioTest123
ENV RUST_BACKTRACE=1
ENV RUST_LOG=debug

# Set up a script to install Rust and build tools after Windows boots
COPY docker/windows/setup.ps1 /oem/install.bat

# Copy the project source code and test scripts
COPY . /data/project
COPY docker/windows/test.ps1 /data/test.ps1

# The dockurr/windows container will handle the Windows installation
# Our setup script will run after installation is complete 