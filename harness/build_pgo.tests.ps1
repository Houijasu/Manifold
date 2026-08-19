$ErrorActionPreference = 'Stop'

$scriptPath = Join-Path $PSScriptRoot 'build_pgo.ps1'
$command = Get-Command $scriptPath
if ($command.Parameters.ContainsKey('PgoDir')) {
    throw 'build_pgo.ps1 must not accept an unsafe PgoDir override'
}

. $scriptPath

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$expectedPgo = Join-Path $root 'target\pgo'
$unsafePgo = Join-Path $root 'target\release'
try {
    Assert-ExactPath -ActualPath $unsafePgo -ExpectedPath $expectedPgo -Description 'PGO publication directory'
    throw 'unsafe PGO path must be rejected'
} catch {
    if ($_.Exception.Message -eq 'unsafe PGO path must be rejected') { throw }
    if ($_.Exception.Message -notmatch 'must resolve exactly') { throw }
}

try {
    Assert-NativeSuccess -NativeExitCode 17 -Description 'NPS comparison' -FailureExitCode 6
    throw 'native failure must be rejected'
} catch {
    if ($_.Exception.Message -eq 'native failure must be rejected') { throw }
    if ($_.Exception.Data['ExitCode'] -ne 6) { throw 'native failure used the wrong exit code' }
}

Assert-BuildInputsMatchHead -GetStatus {
    [pscustomobject]@{ ExitCode = 0; Lines = @() }
}
try {
    Assert-BuildInputsMatchHead -GetStatus {
        [pscustomobject]@{
            ExitCode = 0
            Lines = @(' M crates/mf-search/src/lib.rs', '?? crates/new.rs')
        }
    }
    throw 'dirty build inputs must be rejected'
} catch {
    if ($_.Exception.Message -eq 'dirty build inputs must be rejected') { throw }
    if ($_.Exception.Message -notmatch 'build inputs differ from HEAD') { throw }
    if ($_.Exception.Data['ExitCode'] -ne 5) { throw 'dirty build inputs used the wrong exit code' }
}

$hadCargoTargetDir = Test-Path Env:CARGO_TARGET_DIR
$savedCargoTargetDir = $env:CARGO_TARGET_DIR
$hadRustFlags = Test-Path Env:RUSTFLAGS
$savedRustFlags = $env:RUSTFLAGS
$hadEncodedRustFlags = Test-Path Env:CARGO_ENCODED_RUSTFLAGS
$savedEncodedRustFlags = $env:CARGO_ENCODED_RUSTFLAGS
try {
    $env:CARGO_TARGET_DIR = 'caller-target'
    $env:RUSTFLAGS = 'caller-flags'
    $env:CARGO_ENCODED_RUSTFLAGS = 'caller-encoded-flags'
    try {
        Invoke-WithRestoredBuildEnvironment {
            if (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) {
                throw 'CARGO_ENCODED_RUSTFLAGS was not cleared inside the build environment'
            }
            $env:CARGO_TARGET_DIR = 'inner-target'
            $env:RUSTFLAGS = 'inner-flags'
            $env:CARGO_ENCODED_RUSTFLAGS = 'inner-encoded-flags'
            throw 'expected test failure'
        }
    } catch {
        if ($_.Exception.Message -ne 'expected test failure') { throw }
    }
    if ($env:CARGO_TARGET_DIR -ne 'caller-target') {
        throw 'caller CARGO_TARGET_DIR was not restored after failure'
    }
    if ($env:RUSTFLAGS -ne 'caller-flags') {
        throw 'caller RUSTFLAGS was not restored after failure'
    }
    if ($env:CARGO_ENCODED_RUSTFLAGS -ne 'caller-encoded-flags') {
        throw 'caller CARGO_ENCODED_RUSTFLAGS was not restored after failure'
    }

    Remove-Item Env:CARGO_TARGET_DIR
    Remove-Item Env:RUSTFLAGS
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS
    Invoke-WithRestoredBuildEnvironment {
        if (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) {
            throw 'initially absent CARGO_ENCODED_RUSTFLAGS appeared inside the build environment'
        }
        $env:CARGO_TARGET_DIR = 'inner-target'
        $env:RUSTFLAGS = 'inner-flags'
        $env:CARGO_ENCODED_RUSTFLAGS = 'inner-encoded-flags'
    }
    if (Test-Path Env:CARGO_TARGET_DIR) {
        throw 'previously absent CARGO_TARGET_DIR was not removed after success'
    }
    if (Test-Path Env:RUSTFLAGS) {
        throw 'previously absent RUSTFLAGS was not removed after success'
    }
    if (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) {
        throw 'previously absent CARGO_ENCODED_RUSTFLAGS was not removed after success'
    }
} finally {
    if ($hadCargoTargetDir) {
        $env:CARGO_TARGET_DIR = $savedCargoTargetDir
    } else {
        Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    }
    if ($hadRustFlags) {
        $env:RUSTFLAGS = $savedRustFlags
    } else {
        Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
    }
    if ($hadEncodedRustFlags) {
        $env:CARGO_ENCODED_RUSTFLAGS = $savedEncodedRustFlags
    } else {
        Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    }
}

$testRoot = Join-Path $root 'target\pgo-script-tests'
$staging = Join-Path $testRoot 'staging'
$final = Join-Path $testRoot 'final'
$backup = Join-Path $testRoot 'backup'

Remove-Item $testRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $staging | Out-Null
New-Item -ItemType Directory -Path $final | Out-Null
Set-Content (Join-Path $staging 'new.txt') 'new'
Set-Content (Join-Path $final 'old.txt') 'old'
$moveCalls = [ref]0
try {
    try {
        Publish-PgoStaging -StagingDirectory $staging -FinalDirectory $final `
            -BackupDirectory $backup -ValidatePublished {} -MovePath {
                param([string]$LiteralPath, [string]$Destination)
                $moveCalls.Value++
                throw 'initial backup move failed'
            }
        throw 'initial backup failure must throw'
    } catch {
        if ($_.Exception.Message -eq 'initial backup failure must throw') { throw }
        if ($_.Exception.Message -ne 'initial backup move failed') { throw }
    }
    if (-not (Test-Path (Join-Path $final 'old.txt'))) {
        throw 'initial backup failure deleted the old publication'
    }
    if (Test-Path $staging) {
        throw 'initial backup failure leaked staging'
    }
    if (Test-Path $backup) {
        throw 'initial backup failure left a backup'
    }
    if ($moveCalls.Value -ne 1) {
        throw 'initial backup failure attempted a destructive rollback move'
    }

    Remove-Item $testRoot -Recurse -Force
    New-Item -ItemType Directory -Path $staging | Out-Null
    New-Item -ItemType Directory -Path $final | Out-Null
    Set-Content (Join-Path $staging 'new.txt') 'new'
    Set-Content (Join-Path $final 'old.txt') 'old'
    $moveCalls.Value = 0
    try {
        Publish-PgoStaging -StagingDirectory $staging -FinalDirectory $final `
            -BackupDirectory $backup -ValidatePublished {} -MovePath {
                param([string]$LiteralPath, [string]$Destination)
                $moveCalls.Value++
                if ($moveCalls.Value -eq 2) { throw 'staging move failed' }
                Move-Item -LiteralPath $LiteralPath -Destination $Destination
            }
        throw 'post-backup staging failure must throw'
    } catch {
        if ($_.Exception.Message -eq 'post-backup staging failure must throw') { throw }
        if ($_.Exception.Message -ne 'staging move failed') { throw }
    }
    if (-not (Test-Path (Join-Path $final 'old.txt'))) {
        throw 'post-backup staging failure did not restore the old publication'
    }
    if (Test-Path $staging) {
        throw 'post-backup staging failure leaked staging'
    }
    if (Test-Path $backup) {
        throw 'post-backup staging failure left a backup'
    }
    if ($moveCalls.Value -ne 3) {
        throw 'post-backup staging failure did not perform exactly one restore move'
    }

    Remove-Item $testRoot -Recurse -Force
    New-Item -ItemType Directory -Path $staging | Out-Null
    New-Item -ItemType Directory -Path $final | Out-Null
    Set-Content (Join-Path $staging 'new.txt') 'new'
    Set-Content (Join-Path $final 'old.txt') 'old'
    try {
        Publish-PgoStaging -StagingDirectory $staging -FinalDirectory $final `
            -BackupDirectory $backup -ValidatePublished {} -RemoveBackup {
                param([string]$LiteralPath)
                throw "simulated backup cleanup failure at $LiteralPath"
            }
        throw 'backup cleanup failure must throw'
    } catch {
        if ($_.Exception.Message -eq 'backup cleanup failure must throw') { throw }
        if ($_.Exception.Message -notmatch 'publication committed') { throw }
        if ($_.Exception.Message -notmatch 'backup.*inspection') { throw }
    }
    if (-not (Test-Path (Join-Path $final 'new.txt'))) {
        throw 'backup cleanup failure did not preserve the validated new final'
    }
    if (Test-Path (Join-Path $final 'old.txt')) {
        throw 'backup cleanup failure rolled back to the old final'
    }
    if (-not (Test-Path (Join-Path $backup 'old.txt'))) {
        throw 'backup cleanup failure did not leave the backup remainder'
    }
    if (Test-Path $staging) {
        throw 'backup cleanup failure leaked staging'
    }
} finally {
    Remove-Item $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Remove-Item $testRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $staging | Out-Null
New-Item -ItemType Directory -Path $final | Out-Null
Set-Content (Join-Path $staging 'new.txt') 'new'
Set-Content (Join-Path $final 'old.txt') 'old'
try {
    try {
        Publish-PgoStaging -StagingDirectory $staging -FinalDirectory $final `
            -BackupDirectory $backup -ValidatePublished {
                throw 'publication validation failed'
            }
        throw 'failed publication must throw'
    } catch {
        if ($_.Exception.Message -eq 'failed publication must throw') { throw }
        if ($_.Exception.Message -ne 'publication validation failed') { throw }
    }
    if (-not (Test-Path (Join-Path $final 'old.txt'))) {
        throw 'failed publication did not restore the previous final directory'
    }
    if (Test-Path (Join-Path $final 'new.txt')) {
        throw 'failed publication left new final artifacts'
    }
    if (Test-Path $backup) {
        throw 'failed publication left its backup directory'
    }

    Remove-Item $testRoot -Recurse -Force
    New-Item -ItemType Directory -Path $staging | Out-Null
    Set-Content (Join-Path $staging 'new.txt') 'new'
    try {
        Publish-PgoStaging -StagingDirectory $staging -FinalDirectory $final `
            -BackupDirectory $backup -ValidatePublished {
                throw 'new publication validation failed'
            }
        throw 'failed new publication must throw'
    } catch {
        if ($_.Exception.Message -eq 'failed new publication must throw') { throw }
        if ($_.Exception.Message -ne 'new publication validation failed') { throw }
    }
    foreach ($path in @($staging, $final, $backup)) {
        if (Test-Path $path) {
            throw "failed new publication left artifacts at $path"
        }
    }
} finally {
    Remove-Item $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

$npsTestDirectory = Join-Path $root 'target\pgo-nps-tests'
$npsMetadata = Join-Path $npsTestDirectory 'pgo-metadata.txt'
$npsEvidence = Join-Path $npsTestDirectory 'nps-verdict.txt'
Remove-Item $npsTestDirectory -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $npsTestDirectory | Out-Null
Set-Content $npsMetadata @(
    'nps verdict: pending'
    'nps evidence: pending'
)
try {
    Invoke-NpsComparison -BaselineBinary 'baseline.exe' -OptimizedBinary 'optimized.exe' `
        -MetadataPath $npsMetadata -EvidencePath $npsEvidence -RunComparison {
            [pscustomobject]@{
                Command = 'fake nps comparison'
                Output = @('geometric mean NPS ratio A/B = 0.99x')
                ExitCode = 0
            }
        }
    $passedEvidence = Get-Content $npsEvidence -Raw
    $passedMetadata = Get-Content $npsMetadata -Raw
    if (-not $passedEvidence.Contains('status: passed') -or
        -not $passedEvidence.Contains('exit code: 0') -or
        -not $passedEvidence.Contains('geometric mean NPS ratio A/B = 0.99x')) {
        throw 'successful NPS evidence was incomplete'
    }
    if (-not $passedMetadata.Contains('nps verdict: passed') -or
        -not $passedMetadata.Contains((Get-FileHash $npsEvidence -Algorithm SHA256).Hash)) {
        throw 'successful NPS metadata verdict was incomplete'
    }

    Set-Content $npsMetadata @(
        'nps verdict: pending'
        'nps evidence: pending'
    )
    try {
        Invoke-NpsComparison -BaselineBinary 'baseline.exe' -OptimizedBinary 'optimized.exe' `
            -MetadataPath $npsMetadata -EvidencePath $npsEvidence -RunComparison {
                [pscustomobject]@{
                    Command = 'fake failing nps comparison'
                    Output = @('comparison failed')
                    ExitCode = 19
                }
            }
        throw 'failed NPS comparison must throw'
    } catch {
        if ($_.Exception.Message -eq 'failed NPS comparison must throw') { throw }
        if ($_.Exception.Data['ExitCode'] -ne 6) { throw 'failed NPS comparison used the wrong exit code' }
    }
    if (-not (Get-Content $npsEvidence -Raw).Contains('status: failed')) {
        throw 'failed NPS evidence was not truthful'
    }
    if (-not (Get-Content $npsMetadata -Raw).Contains('nps verdict: failed')) {
        throw 'failed NPS metadata verdict was not truthful'
    }

    Write-NpsNotMeasured -MetadataPath $npsMetadata -EvidencePath $npsEvidence
    if (-not (Get-Content $npsEvidence -Raw).Contains('status: not measured')) {
        throw 'not-measured NPS evidence was missing'
    }
    if (-not (Get-Content $npsMetadata -Raw).Contains('nps verdict: not measured')) {
        throw 'not-measured NPS metadata verdict was missing'
    }
} finally {
    Remove-Item $npsTestDirectory -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output 'build_pgo tests: PASS'
