$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'provenance.ps1')

$root = Get-ManifoldRepositoryRoot
if ($root -ne (Resolve-Path (Join-Path $PSScriptRoot '..')).Path) { throw 'wrong root' }

$inside = Get-BinarySourceAttestation (Join-Path $root 'target\release\manifold.exe')
if ($inside.Mode -ne 'inferred-target-worktree') { throw 'target binary not inferred' }
if ($inside.Commit -ne (& git -C $root rev-parse HEAD).Trim()) { throw 'wrong commit' }

$outside = Get-BinarySourceAttestation (Join-Path $env:TEMP 'copied-engine.exe')
if ($outside.Mode -ne 'unattested' -or $outside.Commit -ne 'unknown') {
    throw 'outside binary must stay unattested'
}

$testDirectory = Join-Path $root 'target\provenance-tests'
$binary = Join-Path $testDirectory 'engine.exe'
$sidecar = "$binary.source-commit"
$attestedCommit = '0123456789abcdef0123456789abcdef01234567'

New-Item -ItemType Directory -Path $testDirectory -Force | Out-Null
try {
    [System.IO.File]::WriteAllText($sidecar, $attestedCommit)
    $attested = Get-BinarySourceAttestation $binary
    if ($attested.Mode -ne 'sidecar' -or $attested.Commit -ne $attestedCommit) {
        throw 'exact sidecar must win'
    }

    [System.IO.File]::WriteAllText($sidecar, "$attestedCommit`n")
    $malformed = Get-BinarySourceAttestation $binary
    if ($malformed.Mode -ne 'unattested' -or $malformed.Commit -ne 'unknown') {
        throw 'malformed sidecar must stay unattested'
    }
} finally {
    Remove-Item -LiteralPath $sidecar -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $testDirectory -ErrorAction SilentlyContinue
}

$runMatch = Get-Content -Raw (Join-Path $PSScriptRoot 'run_match.ps1')
if ($runMatch -notmatch "\[string\]\`$Book = ''") { throw 'book default must be repository-relative' }
if ($runMatch -notmatch "\. \(Join-Path \`$PSScriptRoot 'provenance\.ps1'\)") { throw 'helper not loaded' }
if ($runMatch -notmatch '\$root = Get-ManifoldRepositoryRoot') { throw 'run_match root not worktree-aware' }
foreach ($label in @('Driver commit:', 'Source A:', 'Source mode A:', 'SHA-256 A:', 'Source B:', 'Source mode B:', 'SHA-256 B:')) {
    if (-not $runMatch.Contains($label)) { throw "missing metadata label: $label" }
}
if ($runMatch -match '(?m)^Commit:\s') { throw 'generic Commit label must not be emitted' }
