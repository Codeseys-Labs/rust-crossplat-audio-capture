# scripts/gate.ps1 — PowerShell entry point for the local gate (rsac-7e19).
#
# The gate logic lives once, in scripts/gate.sh (bash), so it cannot drift
# between platforms. Git for Windows ships bash, so this wrapper just finds
# it and delegates. Usage mirrors gate.sh:
#   pwsh scripts/gate.ps1              # lint-job replica
#   pwsh scripts/gate.ps1 --full       # + tests, doctests, docs, module-DAG
#   pwsh scripts/gate.ps1 --tests-only # test-job replica only
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
    Write-Error "gate: Git bash not found. Install Git for Windows, then re-run."
    exit 1
}

& $bashExe (Join-Path $PSScriptRoot 'gate.sh') @args
exit $LASTEXITCODE
