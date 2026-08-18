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

$hadCargoTargetDir = Test-Path Env:CARGO_TARGET_DIR
$savedCargoTargetDir = $env:CARGO_TARGET_DIR
try {
    $env:CARGO_TARGET_DIR = 'caller-target'
    try {
        Invoke-WithRestoredBuildEnvironment {
            $env:CARGO_TARGET_DIR = 'inner-target'
            throw 'expected test failure'
        }
    } catch {
        if ($_.Exception.Message -ne 'expected test failure') { throw }
    }
    if ($env:CARGO_TARGET_DIR -ne 'caller-target') {
        throw 'caller CARGO_TARGET_DIR was not restored after failure'
    }

    Remove-Item Env:CARGO_TARGET_DIR
    Invoke-WithRestoredBuildEnvironment {
        $env:CARGO_TARGET_DIR = 'inner-target'
    }
    if (Test-Path Env:CARGO_TARGET_DIR) {
        throw 'previously absent CARGO_TARGET_DIR was not removed after success'
    }
} finally {
    if ($hadCargoTargetDir) {
        $env:CARGO_TARGET_DIR = $savedCargoTargetDir
    } else {
        Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
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

Write-Output 'build_pgo tests: PASS'
