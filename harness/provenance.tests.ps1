$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'provenance.ps1')

$root = Get-ManifoldRepositoryRoot
if ($root -ne (Resolve-Path (Join-Path $PSScriptRoot '..')).Path) { throw 'wrong root' }

$head = Get-ManifoldHeadCommit
if ($head -ne (& git -C $root rev-parse HEAD).Trim()) { throw 'wrong validated HEAD' }

$inside = Get-BinarySourceAttestation (Join-Path $root 'target\release\manifold.exe')
if ($inside.Mode -ne 'inferred-target-worktree') { throw 'target binary not inferred' }
if ($inside.Commit -ne $head) { throw 'wrong commit' }

$outside = Get-BinarySourceAttestation (Join-Path $env:TEMP 'copied-engine.exe')
if ($outside.Mode -ne 'unattested' -or $outside.Commit -ne 'unknown') {
    throw 'outside binary must stay unattested'
}

$testDirectory = Join-Path $root 'target\provenance-tests'
$binary = Join-Path $testDirectory 'engine.exe'
$sidecar = "$binary.source-commit"
$attestedCommit = '0123456789abcdef0123456789abcdef01234567'
$fakeGit = Join-Path $testDirectory 'fake-git.cmd'

New-Item -ItemType Directory -Path $testDirectory -Force | Out-Null
try {
    [System.IO.File]::WriteAllText($fakeGit, "@echo $attestedCommit`r`n@exit /b 1`r`n")
    try {
        Get-ManifoldHeadCommit -GitCommand $fakeGit
        throw 'non-zero git exit must fail'
    } catch {
        if ($_.Exception.Message -eq 'non-zero git exit must fail') { throw }
        if ($_.Exception.Message -notmatch 'exited with code 1') { throw 'non-zero git failure was not clear' }
    }

    [System.IO.File]::WriteAllText($fakeGit, "@echo not-a-commit`r`n@exit /b 0`r`n")
    try {
        Get-ManifoldHeadCommit -GitCommand $fakeGit
        throw 'malformed git output must fail'
    } catch {
        if ($_.Exception.Message -eq 'malformed git output must fail') { throw }
        if ($_.Exception.Message -notmatch 'exactly one 40-hex commit') { throw 'malformed git failure was not clear' }
    }

    [System.IO.File]::WriteAllText($fakeGit, "@echo $attestedCommit`r`n@echo 1111111111111111111111111111111111111111`r`n@exit /b 0`r`n")
    try {
        Get-ManifoldHeadCommit -GitCommand $fakeGit
        throw 'multiple git results must fail'
    } catch {
        if ($_.Exception.Message -eq 'multiple git results must fail') { throw }
        if ($_.Exception.Message -notmatch 'exactly one 40-hex commit') { throw 'multiple git results failure was not clear' }
    }

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
    Remove-Item -LiteralPath $fakeGit -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $sidecar -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $testDirectory -ErrorAction SilentlyContinue
}

$metadata = Format-ManifoldMatchProvenanceMetadata `
    -DriverCommit '1111111111111111111111111111111111111111' `
    -AName 'engine-a' -ACmd 'a.exe' `
    -SourceA ([pscustomobject]@{ Commit = '2222222222222222222222222222222222222222'; Mode = 'sidecar' }) `
    -ShaA 'AAAA' `
    -BName 'engine-b' -BCmd 'b.exe' `
    -SourceB ([pscustomobject]@{ Commit = '3333333333333333333333333333333333333333'; Mode = 'unattested' }) `
    -ShaB 'BBBB'
foreach ($label in @('Driver commit:', 'Source A:', 'Source mode A:', 'SHA-256 A:', 'Source B:', 'Source mode B:', 'SHA-256 B:')) {
    if (-not $metadata.Contains($label)) { throw "missing metadata label: $label" }
}
if ($metadata -match '(?m)^Commit:\s') { throw 'generic Commit label must not be emitted' }
