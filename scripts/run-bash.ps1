# scripts/run-bash.ps1 — generic "run a bash script with Git bash" wrapper.
#
# The repo's script logic lives once, in bash, so it cannot drift between
# platforms (the gate.sh principle). Git for Windows ships bash, so on
# Windows this wrapper finds it and delegates — for ANY script, so each new
# mise task doesn't need its own copy of the bash-discovery logic.
#
# Usage:
#   pwsh scripts/run-bash.ps1 scripts/bump-version.sh 0.5.0 --dry-run
#   pwsh scripts/run-bash.ps1 scripts/verify-docs-rs.sh
param(
    [Parameter(Mandatory = $true, Position = 0)][string]$Script,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest
)
$ErrorActionPreference = 'Stop'

# Prefer Git for Windows' bash explicitly. A bare `bash` on PATH often
# resolves to WSL's System32\bash.exe, which is a different OS (wrong
# backend feature, no cargo on PATH).
$candidates = @(
    (Join-Path $env:ProgramFiles 'Git\bin\bash.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Git\bin\bash.exe'),
    (Join-Path $env:LOCALAPPDATA 'Programs\Git\bin\bash.exe')
) | Where-Object { $_ -and (Test-Path $_) }

$bashExe = $candidates | Select-Object -First 1
if (-not $bashExe) {
    $cmd = Get-Command bash -ErrorAction SilentlyContinue
    if ($cmd -and $cmd.Source -notmatch 'System32') { $bashExe = $cmd.Source }
}
if (-not $bashExe) {
    Write-Error "run-bash: Git bash not found. Install Git for Windows, then re-run."
    exit 1
}

& $bashExe $Script @Rest
exit $LASTEXITCODE
