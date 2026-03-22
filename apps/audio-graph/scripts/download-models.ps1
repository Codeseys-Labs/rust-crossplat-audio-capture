#Requires -Version 5.1
<#
.SYNOPSIS
    Download ML model files for AudioGraph.

.DESCRIPTION
    Downloads Whisper GGML model (and optionally LFM2 sidecar model) into the
    models/ directory.  Existing files are skipped (idempotent).

.PARAMETER WithSidecar
    Also download the LFM2-350M-Extract GGUF model for entity extraction.

.EXAMPLE
    .\scripts\download-models.ps1
    # Download Whisper model only

.EXAMPLE
    .\scripts\download-models.ps1 -WithSidecar
    # Also download LFM2 sidecar model
#>

[CmdletBinding()]
param(
    [switch]$WithSidecar
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Resolve project root (parent of the scripts/ directory)
# ---------------------------------------------------------------------------
$ScriptDir   = Split-Path -Parent $MyInvocation.MyCommand.Definition
$ProjectRoot = Split-Path -Parent $ScriptDir
$ModelsDir   = Join-Path $ProjectRoot 'models'

# ---------------------------------------------------------------------------
# Helper: human-readable file size
# ---------------------------------------------------------------------------
function Format-FileSize([long]$Bytes) {
    if ($Bytes -ge 1GB) { return '{0:N1} GB' -f ($Bytes / 1GB) }
    if ($Bytes -ge 1MB) { return '{0:N1} MB' -f ($Bytes / 1MB) }
    return '{0:N1} KB' -f ($Bytes / 1KB)
}

# ---------------------------------------------------------------------------
# Helper: download with progress
# ---------------------------------------------------------------------------
function Download-Model {
    param(
        [string]$Url,
        [string]$Destination,
        [string]$Label
    )

    if (Test-Path $Destination) {
        Write-Host "  [SKIP] $Label - already exists" -ForegroundColor Yellow
        return $false
    }

    Write-Host "  Downloading $Label..." -ForegroundColor Cyan
    Write-Host "  URL: $Url" -ForegroundColor DarkGray

    # Use BITS transfer if available (shows progress), fall back to Invoke-WebRequest
    try {
        $ProgressPreference = 'Continue'
        Invoke-WebRequest -Uri $Url -OutFile $Destination -UseBasicParsing
    }
    catch {
        Remove-Item -Path $Destination -ErrorAction SilentlyContinue
        throw "Download failed for ${Label}: $_"
    }

    # Verify
    if (-not (Test-Path $Destination)) {
        throw "Download failed: $Label not found at $Destination"
    }

    $Size = (Get-Item $Destination).Length
    if ($Size -eq 0) {
        Remove-Item -Path $Destination -Force
        throw "Download failed: $Label is empty (0 bytes)"
    }

    $HumanSize = Format-FileSize $Size
    Write-Host "  [OK]   $Label ($HumanSize)" -ForegroundColor Green
    return $true
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host 'AudioGraph Model Downloader' -ForegroundColor White
Write-Host ('=' * 35)
Write-Host ''

# Create models directory
if (-not (Test-Path $ModelsDir)) {
    New-Item -ItemType Directory -Path $ModelsDir -Force | Out-Null
    Write-Host "  Created models directory: $ModelsDir" -ForegroundColor Cyan
}
else {
    Write-Host "  Models directory: $ModelsDir" -ForegroundColor Cyan
}
Write-Host ''

$Downloaded = @()
$Skipped    = @()

# --- Whisper model -----------------------------------------------------------
$WhisperUrl   = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin'
$WhisperFile  = Join-Path $ModelsDir 'ggml-small.en.bin'
$WhisperLabel = 'Whisper small.en (GGML)'

$result = Download-Model -Url $WhisperUrl -Destination $WhisperFile -Label $WhisperLabel
if ($result) { $Downloaded += $WhisperLabel } else { $Skipped += $WhisperLabel }

# --- LFM2 sidecar model (optional) ------------------------------------------
if ($WithSidecar) {
    Write-Host ''
    $SidecarUrl   = 'https://huggingface.co/QuantFactory/LFM2-350M-Extract-GGUF/resolve/main/LFM2-350M-Extract.Q8_0.gguf'
    $SidecarFile  = Join-Path $ModelsDir 'LFM2-350M-Extract.Q8_0.gguf'
    $SidecarLabel = 'LFM2-350M-Extract (GGUF Q8_0)'

    $result = Download-Model -Url $SidecarUrl -Destination $SidecarFile -Label $SidecarLabel
    if ($result) { $Downloaded += $SidecarLabel } else { $Skipped += $SidecarLabel }
}

# --- Summary -----------------------------------------------------------------
Write-Host ''
Write-Host ('=' * 35)
Write-Host 'Summary' -ForegroundColor White
Write-Host ''

foreach ($item in $Downloaded) {
    Write-Host "  [OK]   Downloaded: $item" -ForegroundColor Green
}
foreach ($item in $Skipped) {
    Write-Host "  [SKIP] Skipped:    $item" -ForegroundColor Yellow
}

Write-Host ''
Write-Host '  Models directory contents:' -ForegroundColor Cyan
Get-ChildItem -Path $ModelsDir -File | ForEach-Object {
    $sz = Format-FileSize $_.Length
    Write-Host "    $($_.Name)  ($sz)"
}
Write-Host ''

if (-not $WithSidecar) {
    Write-Host '  Tip: Run with -WithSidecar to also download the LFM2 entity extraction model.' -ForegroundColor DarkGray
}

Write-Host '  Done!' -ForegroundColor Green
Write-Host ''
