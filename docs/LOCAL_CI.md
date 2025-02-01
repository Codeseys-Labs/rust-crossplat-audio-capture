# Running GitHub Actions Locally

This guide explains how to run and test GitHub Actions workflows locally before pushing to the repository.

## Prerequisites

1. Install `act`:
   ```bash
   # macOS
   brew install act

   # Linux
   curl https://raw.githubusercontent.com/nektos/act/master/install.sh | sudo bash

   # Windows (with Chocolatey)
   choco install act-cli
   ```

2. Install Docker (required by `act`)

## Basic Usage

### List Available Actions
```bash
# List all available actions
act -l

# Expected output:
# Stage  Job ID        Job name                 Workflow name        Events
# 0      test-windows  test-windows             Windows Audio Tests  workflow_call
# 0      test-linux    test-linux               Linux Audio Tests    workflow_call
# 0      test-macos    test-macos               macOS Audio Tests    workflow_call
```

### Running Specific Workflows

1. **Windows Tests**:
   ```bash
   # Run Windows workflow
   act -j test-windows -P ubuntu-latest=nektos/act-environments-ubuntu:18.04

   # With debug enabled
   act -j test-windows -P ubuntu-latest=nektos/act-environments-ubuntu:18.04 \
       --input debug_enabled=true
   ```

2. **Linux Tests**:
   ```bash
   # Run Linux workflow
   act -j test-linux -P ubuntu-latest=nektos/act-environments-ubuntu:18.04

   # Test specific audio backend
   act -j test-linux -P ubuntu-latest=nektos/act-environments-ubuntu:18.04 \
       --input audio_backend=pipewire
   ```

3. **macOS Tests**:
   ```bash
   # Run macOS workflow
   act -j test-macos -P ubuntu-latest=nektos/act-environments-ubuntu:18.04
   ```

## Environment Setup

### Audio Dependencies

1. **Windows Dependencies**:
   ```bash
   # Create .actrc file
   echo "WINDOWS_SDK_VERSION=18362" > .actrc
   ```

2. **Linux Dependencies**:
   ```bash
   # Create a custom Dockerfile for Linux tests
   cat > Dockerfile.linux << 'EOF'
   FROM nektos/act-environments-ubuntu:18.04

   RUN apt-get update && apt-get install -y \
       alsa-utils \
       libasound2-dev \
       pulseaudio \
       pulseaudio-utils \
       pipewire \
       pipewire-pulse \
       pipewire-alsa \
       pipewire-audio \
       wireplumber

   EOF

   # Use custom image
   act -j test-linux -P ubuntu-latest=Dockerfile.linux
   ```

3. **macOS Dependencies**:
   ```bash
   # Create a custom Dockerfile for macOS tests
   cat > Dockerfile.macos << 'EOF'
   FROM nektos/act-environments-ubuntu:18.04

   RUN apt-get update && apt-get install -y \
       clang \
       llvm \
       pkg-config

   EOF

   # Use custom image
   act -j test-macos -P ubuntu-latest=Dockerfile.macos
   ```

## Advanced Usage

### Running with Artifacts
```bash
# Create artifacts directory
mkdir -p /tmp/artifacts

# Run with artifact collection
act -j test-windows \
    -P ubuntu-latest=nektos/act-environments-ubuntu:18.04 \
    --artifact-server-path /tmp/artifacts
```

### Debug Mode
```bash
# Run with debug logging
act -j test-windows \
    -P ubuntu-latest=nektos/act-environments-ubuntu:18.04 \
    -v
```

### Matrix Testing
```bash
# Run Linux tests with matrix
act -j test-linux \
    -P ubuntu-latest=Dockerfile.linux \
    --matrix audio_backend:[pipewire,pulseaudio]
```

## Common Issues and Solutions

### 1. Resource Limits
If you encounter resource limits:
```bash
# Increase Docker memory limit
docker run --memory 4g --cpus 2 ...
```

### 2. Missing Dependencies
If dependencies are missing:
```bash
# Update custom Dockerfile
cat >> Dockerfile.custom << 'EOF'
RUN apt-get update && apt-get install -y \
    package-name-here
EOF
```

### 3. Permission Issues
For permission-related problems:
```bash
# Run with elevated privileges
act -j test-linux \
    -P ubuntu-latest=Dockerfile.linux \
    --privileged
```

## Best Practices

1. **Local Testing First**:
   ```bash
   # Test locally before pushing
   act -l  # List workflows
   act -n  # Dry run
   act     # Run all workflows
   ```

2. **Workflow Isolation**:
   ```bash
   # Test specific job
   act -j job-name

   # Test specific event
   act pull_request
   ```

3. **Resource Management**:
   ```bash
   # Clean up after testing
   docker system prune -f
   rm -rf /tmp/artifacts/*
   ```

## Useful Commands

```bash
# List all workflows
act -l

# Dry run
act -n

# Run with specific event
act pull_request

# Run with specific platform
act -P ubuntu-latest=nektos/act-environments-ubuntu:18.04

# Run with secrets
act --secret-file my.secrets

# Run with specific working directory
act -C path/to/repo
```

## Additional Resources

- [act GitHub Repository](https://github.com/nektos/act)
- [act Documentation](https://github.com/nektos/act#readme)
- [Docker Documentation](https://docs.docker.com/)
- [GitHub Actions Documentation](https://docs.github.com/en/actions)