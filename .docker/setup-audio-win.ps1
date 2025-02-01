# Setup script for Windows audio in container
Write-Host "Setting up Windows audio environment..."

# Configure Windows Audio service
Set-Service -Name Audiosrv -StartupType Automatic
Start-Service Audiosrv

# Wait for service to be ready
$retries = 5
$serviceReady = $false

for ($i = 0; $i -lt $retries; $i++) {
    $audioService = Get-Service Audiosrv
    if ($audioService.Status -eq 'Running') {
        $serviceReady = $true
        break
    }
    Write-Host "Waiting for Audio service to start... Attempt $($i + 1)"
    Start-Sleep -Seconds 2
}

if (-not $serviceReady) {
    throw "Failed to start Audio service after $retries attempts"
}

Write-Host "Audio service configured successfully"

# Execute the passed command
$args