# scripts/gate.ps1 — PowerShell entry point for the local gate (rsac-7e19).
#
# The gate logic lives once, in scripts/gate.sh (bash), so it cannot drift
# between platforms; the Git-bash discovery likewise lives once, in
# scripts/run-bash.ps1. Usage mirrors gate.sh:
#   pwsh scripts/gate.ps1              # lint-job replica
#   pwsh scripts/gate.ps1 --full       # + tests, doctests, docs, module-DAG
#   pwsh scripts/gate.ps1 --tests-only # test-job replica only
$ErrorActionPreference = 'Stop'

& (Join-Path $PSScriptRoot 'run-bash.ps1') (Join-Path $PSScriptRoot 'gate.sh') @args
exit $LASTEXITCODE
