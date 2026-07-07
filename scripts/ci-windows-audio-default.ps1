# =============================================================================
# Deterministic VB-CABLE default-endpoint gate for the Windows audio CI jobs
# (seed rsac-0f33).
#
# Called by: .github/workflows/ci-audio-tests.yml (windows-system,
#            windows-device, windows-process), after LABSN/sound-ci-helpers
#            has installed the VB-CABLE driver.
#
# What it does, in order:
#
#   1. Sets VB-CABLE as the DEFAULT PLAYBACK endpoint via AudioDeviceCmdlets,
#      with bounded retries (rsac-eb2f: the endpoint can take seconds to
#      register with AudioSrv after driver install; rsac#24: LABSN installs
#      the driver but does not set the default).
#   2. HARD-VERIFIES the active default playback is VB-CABLE. If it is not,
#      the test tone would route to the wrong endpoint and every loopback
#      capture would see silence — fail loudly here instead of wasting the
#      cargo build + 15 minutes of tests that cannot pass.
#   3. Exports RSAC_CI_AUDIO_AVAILABLE=1 and RSAC_CI_AUDIO_DETERMINISTIC=1.
#
# Why the deterministic flip is sound for ALL THREE Windows tiers (not just
# system capture): every ci_audio content assertion consumes tone played by a
# test-spawned player to the DEFAULT playback endpoint, which this script has
# just hard-verified to be VB-CABLE:
#   - system tier:  SystemDefault loopback of that same endpoint;
#   - device tier:  the tests target enumerator.default_device() — the very
#                   endpoint verified here — and spawn their own tone player;
#   - process tier: WASAPI process loopback of the spawned player process,
#                   whose PlayLooping output routes to the verified endpoint.
# (The application_by_name_windows module keeps its content asserts soft by
# its own module-documented design regardless of this env.)
# =============================================================================
$ErrorActionPreference = "Stop"

Install-Module -Name AudioDeviceCmdlets -Force -Scope CurrentUser 2>$null
Import-Module AudioDeviceCmdlets -ErrorAction Stop

# ── 1. Set VB-CABLE as the default playback endpoint (bounded retries) ──────
$cable = $null
$maxAttempts = 6
for ($attempt = 1; $attempt -le $maxAttempts; $attempt++) {
    Write-Host "=== Audio device discovery (attempt $attempt/$maxAttempts) ==="
    $devices = Get-AudioDevice -List
    $devices | Format-Table Index, Type, Name, Default -AutoSize

    # On windows-latest VB-CABLE's playback endpoint is named
    # "Speakers (VB-Audio Virtual Cable)", not "CABLE Input ...", so the
    # filter accepts either form (rsac#24).
    $cable = $devices | Where-Object {
        $_.Type -eq "Playback" -and (
            $_.Name -like "*CABLE Input*" -or
            $_.Name -like "*VB-Audio*" -or
            $_.Name -like "*VB-CABLE*"
        )
    } | Select-Object -First 1

    if ($cable) {
        try {
            Write-Host "Setting VB-CABLE as default playback device: $($cable.Name) (Index=$($cable.Index))"
            Set-AudioDevice -Index $cable.Index -DefaultOnly
            break
        } catch {
            Write-Host "Set-AudioDevice failed on attempt ${attempt}: $_"
            $cable = $null
        }
    }
    if ($attempt -lt $maxAttempts) { Start-Sleep -Seconds 5 }
}

if (-not $cable) {
    Write-Error "No VB-CABLE playback device found after $maxAttempts attempts - LABSN install likely failed"
    exit 1
}

# ── 2. Hard-verify the active default playback ─────────────────────────────
$playback = Get-AudioDevice -Playback
Write-Host "Default playback: $($playback.Name) (Default=$($playback.Default), Type=$($playback.Type))"
$isCable = ($playback.Name -like "*VB-Audio*") -or
           ($playback.Name -like "*CABLE*") -or
           ($playback.Name -like "*VB-CABLE*")
if (-not $isCable) {
    Write-Error "Default playback is NOT VB-CABLE (got: '$($playback.Name)'). Loopback capture cannot succeed."
    exit 1
}
Write-Host "OK: VB-CABLE is the verified default playback endpoint."

# Endpoint inventory for the logs (diagnostic only — the hard gate above is
# the authority).
Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object {
    $_.FriendlyName -like "*VB-Audio*" -or
    $_.FriendlyName -like "*CABLE*" -or
    $_.Class -eq "AudioEndpoint" -or
    $_.Class -eq "MEDIA"
} | Format-List FriendlyName, Status

# ── 3. Enable the audio tests' hard content assertions ─────────────────────
if ($env:GITHUB_ENV) {
    "RSAC_CI_AUDIO_AVAILABLE=1" >> $env:GITHUB_ENV
    "RSAC_CI_AUDIO_DETERMINISTIC=1" >> $env:GITHUB_ENV
    Write-Host "exported RSAC_CI_AUDIO_AVAILABLE=1 and RSAC_CI_AUDIO_DETERMINISTIC=1 to `$GITHUB_ENV"
} else {
    Write-Host "no `$GITHUB_ENV - set these yourself:"
    Write-Host "  `$env:RSAC_CI_AUDIO_AVAILABLE = '1'"
    Write-Host "  `$env:RSAC_CI_AUDIO_DETERMINISTIC = '1'"
}
