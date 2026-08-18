$ErrorActionPreference = 'Stop'

$scriptPath = Join-Path $PSScriptRoot 'build_portable.ps1'
if (-not (Test-Path -LiteralPath $scriptPath)) {
    throw 'build_portable.ps1 is missing'
}

$source = Get-Content -LiteralPath $scriptPath -Raw
[void][scriptblock]::Create($source)
. $scriptPath

try {
    Assert-NativeSuccess -NativeExitCode 17 -Description 'portable bench' -FailureExitCode 5
    throw 'native failure must be rejected'
} catch {
    if ($_.Exception.Message -eq 'native failure must be rejected') { throw }
    if ($_.Exception.Data['ExitCode'] -ne 5) { throw 'native failure used the wrong exit code' }
}

if ((Get-NodesSignatureFromOutput 'Nodes searched: 37420' 'bench') -ne 37420) {
    throw 'node signature parser returned the wrong value'
}
try {
    Get-NodesSignatureFromOutput 'no signature here' 'bench'
    throw 'missing node signature must be rejected'
} catch {
    if ($_.Exception.Message -eq 'missing node signature must be rejected') { throw }
    if ($_.Exception.Message -notmatch 'no node signature') { throw }
}

$tokens = @(Get-ForbiddenInstructionTokens @(
    '0000000140001000: c4 e2 e2 f5 c3        pext rax, rbx, rcx'
    '0000000140001004: 48 89 d8              mov rax, rbx # pdep appears only in a comment'
    '0000000140001008 <bzhi_metadata>:'
    '000000014000100c: c4 e3 fb f0 c3 07     rorx rax, rbx, 7'
))
if (($tokens -join ',') -ne 'pext,rorx') {
    throw "instruction scan did not isolate mnemonic tokens: $($tokens -join ',')"
}

$hadCargoTargetDir = Test-Path Env:CARGO_TARGET_DIR
$savedCargoTargetDir = $env:CARGO_TARGET_DIR
$hadRustFlags = Test-Path Env:RUSTFLAGS
$savedRustFlags = $env:RUSTFLAGS
try {
    $env:CARGO_TARGET_DIR = 'caller-target'
    $env:RUSTFLAGS = 'caller-flags'
    try {
        Invoke-WithRestoredBuildEnvironment {
            $env:CARGO_TARGET_DIR = 'inner-target'
            $env:RUSTFLAGS = 'inner-flags'
            throw 'expected test failure'
        }
    } catch {
        if ($_.Exception.Message -ne 'expected test failure') { throw }
    }
    if ($env:CARGO_TARGET_DIR -ne 'caller-target') {
        throw 'caller CARGO_TARGET_DIR was not restored'
    }
    if ($env:RUSTFLAGS -ne 'caller-flags') {
        throw 'caller RUSTFLAGS was not restored'
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
}

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$testRoot = Join-Path $root 'target\portable-script-tests'
$staging = Join-Path $testRoot 'staging'
$final = Join-Path $testRoot 'final'
$backup = Join-Path $testRoot 'backup'
Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $staging | Out-Null
New-Item -ItemType Directory -Path $final | Out-Null
Set-Content (Join-Path $staging 'new.txt') 'new'
Set-Content (Join-Path $final 'old.txt') 'old'
try {
    try {
        Publish-PortableStaging -StagingDirectory $staging -FinalDirectory $final `
            -BackupDirectory $backup -ValidatePublished {
                throw 'publication validation failed'
            }
        throw 'failed publication must throw'
    } catch {
        if ($_.Exception.Message -eq 'failed publication must throw') { throw }
        if ($_.Exception.Message -ne 'publication validation failed') { throw }
    }
    if (-not (Test-Path (Join-Path $final 'old.txt'))) {
        throw 'failed publication did not restore the previous artifacts'
    }
    if (Test-Path (Join-Path $final 'new.txt')) {
        throw 'failed publication left incomplete new artifacts'
    }
    if (Test-Path -LiteralPath $backup) {
        throw 'failed publication left its backup directory'
    }
} finally {
    Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output 'build_portable tests: PASS'
