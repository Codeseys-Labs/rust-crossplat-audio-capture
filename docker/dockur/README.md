# Dockur Native OS Testing

Run full Windows and macOS virtual machines inside Docker for **native** WASAPI and CoreAudio testing of the `rsac` library. This uses [dockur/windows](https://github.com/dockur/windows) and [dockur/macos](https://github.com/dockur/macos), which wrap QEMU/KVM inside Docker containers.

## What is Dockur?

Dockur runs complete operating system installations (Windows, macOS) as VMs inside Docker, using QEMU with KVM hardware acceleration. Unlike cross-compilation (which only verifies that code compiles), dockur VMs let you **run** the compiled binaries on real Windows/macOS kernels with real audio subsystems (WASAPI, CoreAudio).

## Requirements

| Requirement | Detail |
|---|---|
| **Linux host** | Required — dockur uses QEMU/KVM |
| **KVM support** | `/dev/kvm` must be accessible (`ls -la /dev/kvm`) |
| **RAM** | ~8 GB per VM (configurable) |
| **Disk** | ~64 GB per VM for OS install + storage |
| **Docker** | Docker Engine with Compose v2 |
| **Audio** (optional) | PulseAudio or PipeWire on host for audio pass-through |

### Verify KVM

```bash
# Check KVM device exists
ls -la /dev/kvm

# Check KVM kernel module
lsmod | grep kvm
```

If `/dev/kvm` doesn't exist, enable virtualization in BIOS and load the kernel module:
```bash
sudo modprobe kvm_intel   # Intel CPUs
sudo modprobe kvm_amd     # AMD CPUs
```

## Quick Start

### Start Windows VM

```bash
docker compose -f docker-compose.native-testing.yml up windows-native -d
```

### Start macOS VM

```bash
docker compose -f docker-compose.native-testing.yml up macos-native -d
```

### Start Both

```bash
docker compose -f docker-compose.native-testing.yml up -d
```

> **First boot takes 15–30 minutes** while the OS installs automatically. Subsequent boots are fast (~1 minute).

## Connecting to the VMs

### Windows VM

| Method | URL / Address |
|---|---|
| **Web console (noVNC)** | http://localhost:8006 |
| **RDP** | `localhost:3389` (user: `RustDev`, pass: `AudioTest123`) |

### macOS VM

| Method | URL / Address |
|---|---|
| **Web console (noVNC)** | http://localhost:8007 |
| **VNC** | `localhost:5900` |

## File Sharing

### Windows

The project root is automatically mapped as a network share inside the Windows VM:
```
\\host.lan\Data  →  project root
```

The OEM install script maps this to the `Z:` drive automatically. If not mapped:
```cmd
net use Z: \\host.lan\Data /persistent:yes
```

### macOS

Mount the shared folder using the 9p filesystem:
```bash
sudo mkdir -p /Volumes/shared
sudo mount_9p shared /Volumes/shared
```

The project will be available at `/Volumes/shared`.

## Audio Setup

### Windows — Virtual Intel HDA Sound Card

The docker-compose file passes QEMU arguments to create a virtual Intel HDA audio device:

```
-audiodev pa,id=snd0 -device intel-hda -device hda-duplex,audiodev=snd0
```

This creates a sound card visible to Windows, which means:
- Windows Audio Service (`Audiosrv`) starts normally
- WASAPI enumerates the virtual device
- Audio capture/render APIs work against the virtual device

**Audio backend options** (configured via the `ARGUMENTS` environment variable):

| Host Audio | QEMU Argument | Result |
|---|---|---|
| PulseAudio | `-audiodev pa,id=snd0` | Full audio pass-through to host |
| PipeWire (with PulseAudio compat) | `-audiodev pa,id=snd0` | Works via PipeWire's PA socket |
| No host audio daemon | `-audiodev none,id=snd0` | WASAPI still enumerates device; no actual audio output |

### macOS — CoreAudio

macOS VMs use the system's built-in CoreAudio. Audio device availability depends on the QEMU configuration. CoreAudio APIs should be enumerable regardless.

## Running Tests

### Windows

After the VM has booted and the OEM install has completed:

**Option 1: Use the test runner script**
```powershell
# Open PowerShell in the VM, then:
Z:\docker\dockur\windows\test-native.ps1
```

**Option 2: Run manually**
```powershell
cd Z:\
cargo check --features feat_windows
cargo test --features feat_windows -- --test-threads=1
```

### macOS

After the VM has booted:

**Step 1: Run the setup script** (first time only)
```bash
chmod +x /Volumes/shared/docker/dockur/macos/setup.sh
/Volumes/shared/docker/dockur/macos/setup.sh
```

**Step 2: Run the test script**
```bash
chmod +x /Volumes/shared/docker/dockur/macos/test-native.sh
/Volumes/shared/docker/dockur/macos/test-native.sh
```

Or manually:
```bash
cd /Volumes/shared
cargo test --features feat_macos -- --test-threads=1
```

## Comparison with Cross-Compilation

| Aspect | Cross-Compilation (`cargo-xwin`, `osxcross`) | Native VM (dockur) |
|---|---|---|
| **What it tests** | Compilation only | Compilation + execution |
| **Audio APIs** | Stubbed / not available | Real WASAPI / CoreAudio |
| **Speed** | Fast (minutes) | Slow first boot (15–30 min), then fast |
| **Resources** | ~2 GB RAM | ~8 GB RAM + 64 GB disk per VM |
| **KVM required** | No | Yes |
| **CI/CD friendly** | Yes (any Linux CI) | Requires KVM-enabled runners |
| **Use case** | Verify code compiles for target | Verify code *works* on target |

**Recommendation**: Use cross-compilation for CI/CD compilation checks. Use dockur VMs for pre-release native testing and debugging platform-specific audio issues.

## Stopping the VMs

```bash
# Stop gracefully (saves state)
docker compose -f docker-compose.native-testing.yml stop

# Stop and remove containers (VM disk data persists in named volumes)
docker compose -f docker-compose.native-testing.yml down

# Stop, remove containers, AND delete VM disks
docker compose -f docker-compose.native-testing.yml down -v
```

## Known Limitations

1. **Audio capture testing requires an audio source**: To test audio capture, something must be producing audio inside the VM (e.g., play a test tone, video, etc.).
2. **macOS EULA**: Running macOS in a VM may have legal implications depending on your jurisdiction and hardware. Apple's EULA permits macOS VMs only on Apple hardware. Use at your own discretion.
3. **First boot is slow**: OS installation happens on first boot and takes 15–30 minutes. Subsequent boots use the installed OS from the Docker volume.
4. **GPU acceleration**: Not available — the VMs use software rendering. This doesn't affect audio testing.
5. **VNC/RDP latency**: Web console (noVNC) has noticeable latency. Use RDP for Windows when possible.
6. **Disk space**: Each VM needs ~64 GB for the OS install. Ensure sufficient disk space before starting.

## Troubleshooting

### "KVM device not found"

```bash
# Check if KVM module is loaded
lsmod | grep kvm

# Load the appropriate module
sudo modprobe kvm_intel   # or kvm_amd

# Check permissions
ls -la /dev/kvm
# Should be crw-rw---- with group 'kvm'
# Add your user to the kvm group:
sudo usermod -aG kvm $USER
```

### Windows VM stuck at installation

Check the web console at http://localhost:8006. The Windows installer may be waiting for user input. Most installation steps are automated by dockur, but occasionally manual intervention is needed.

### Shared drive not accessible in Windows

```cmd
REM Verify network connectivity
ping host.lan

REM Manually map the drive
net use Z: \\host.lan\Data
```

### macOS shared folder mount fails

```bash
# The mount_9p command requires the 9p kernel extension
# If it fails, try specifying the transport:
sudo mount -t 9p -o trans=virtio shared /Volumes/shared
```

## File Structure

```
docker/dockur/
├── README.md                    # This file
├── windows/
│   ├── oem/
│   │   └── install.bat          # Auto-runs on first Windows boot (via dockur /oem)
│   └── test-native.ps1          # Manual test runner (PowerShell)
└── macos/
    ├── setup.sh                 # One-time setup script
    └── test-native.sh           # Manual test runner (bash)

docker-compose.native-testing.yml   # Compose file at project root
```
