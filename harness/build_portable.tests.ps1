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
    '000000014000100c: c4 e2 d2 f5 c6        pdep rax, rbx, rcx'
    '0000000140001010: c4 e2 e8 f5 c1        bzhi rax, rbx, rcx'
    '0000000140001014: c4 e2 fb f6 c1        mulx rax, rbx, rcx'
    '0000000140001018: c4 e2 ea f7 c1        sarx rax, rbx, rcx'
    '000000014000101c: c4 e2 e9 f7 c1        shlx rax, rbx, rcx'
    '0000000140001020: c4 e2 eb f7 c1        shrx rax, rbx, rcx'
    '0000000140001024: c4 e3 fb f0 c3 07     rorx rax, rbx, 7'
))
if (($tokens -join ',') -ne 'pext,pdep,bzhi,mulx,sarx,shlx,shrx,rorx') {
    throw "instruction scan did not isolate mnemonic tokens: $($tokens -join ',')"
}
$cleanTokens = @(Get-ForbiddenInstructionTokens @(
    '0000000140002000: 48 89 d8              mov rax, rbx'
    '0000000140002004: 48 01 c8              add rax, rcx # pext is comment text'
    '0000000140002008 <rorx_metadata>:'
))
if ($cleanTokens.Count -ne 0) {
    throw "clean instruction fixture produced forbidden tokens: $($cleanTokens -join ',')"
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
        throw 'caller CARGO_TARGET_DIR was not restored'
    }
    if ($env:RUSTFLAGS -ne 'caller-flags') {
        throw 'caller RUSTFLAGS was not restored'
    }
    if ($env:CARGO_ENCODED_RUSTFLAGS -ne 'caller-encoded-flags') {
        throw 'caller CARGO_ENCODED_RUSTFLAGS was not restored'
    }

    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS
    Invoke-WithRestoredBuildEnvironment {
        if (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) {
            throw 'initially absent CARGO_ENCODED_RUSTFLAGS appeared inside the build environment'
        }
        $env:CARGO_ENCODED_RUSTFLAGS = 'inner-encoded-flags'
    }
    if (Test-Path Env:CARGO_ENCODED_RUSTFLAGS) {
        throw 'initially absent CARGO_ENCODED_RUSTFLAGS was not removed after success'
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
    $buildOutput = Join-Path $testRoot 'build-output.exe'
    $stagedBinaryDirectory = Join-Path $testRoot 'binary-staging'
    [System.IO.File]::WriteAllText($buildOutput, 'validated portable bytes')
    $stagedBinary = Stage-PortableBinary $buildOutput $stagedBinaryDirectory
    $stagedHash = (Get-FileHash -LiteralPath $stagedBinary -Algorithm SHA256).Hash
    [System.IO.File]::WriteAllText($buildOutput, 'mutated build output')
    Assert-StableFileHash $stagedBinary $stagedHash 'staged portable binary' | Out-Null
    if ([System.IO.File]::ReadAllText($stagedBinary) -cne 'validated portable bytes') {
        throw 'staged portable binary changed with mutable build output'
    }

    $testNetwork = Join-Path $testRoot 'main.nnue'
    [System.IO.File]::WriteAllText($testNetwork, 'network before build')
    $networkHash = (Get-FileHash -LiteralPath $testNetwork -Algorithm SHA256).Hash
    Assert-StableFileHash $testNetwork $networkHash 'embedded network' | Out-Null
    [System.IO.File]::WriteAllText($testNetwork, 'network changed during build')
    try {
        Assert-StableFileHash $testNetwork $networkHash 'embedded network'
        throw 'network change must be rejected'
    } catch {
        if ($_.Exception.Message -eq 'network change must be rejected') { throw }
        if ($_.Exception.Message -notmatch 'embedded network changed') { throw }
    }

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

    Remove-Item -LiteralPath $testRoot -Recurse -Force
    New-Item -ItemType Directory -Path $staging | Out-Null
    New-Item -ItemType Directory -Path $final | Out-Null
    Set-Content (Join-Path $staging 'new.txt') 'new'
    Set-Content (Join-Path $final 'old.txt') 'old'
    [System.IO.File]::WriteAllText($testNetwork, 'network before publication')
    $networkHash = (Get-FileHash -LiteralPath $testNetwork -Algorithm SHA256).Hash
    [System.IO.File]::WriteAllText($testNetwork, 'network changed at publication')
    try {
        Publish-PortableStaging -StagingDirectory $staging -FinalDirectory $final `
            -BackupDirectory $backup -ValidatePublished {
                Assert-StableFileHash $testNetwork $networkHash 'embedded network' | Out-Null
            }
        throw 'publication-time network change must throw'
    } catch {
        if ($_.Exception.Message -eq 'publication-time network change must throw') { throw }
        if ($_.Exception.Message -notmatch 'embedded network changed') { throw }
    }
    if (-not (Test-Path (Join-Path $final 'old.txt'))) {
        throw 'publication-time network change did not restore the previous final'
    }
    if (Test-Path -LiteralPath $backup) {
        throw 'publication-time network change left its backup directory'
    }

    Remove-Item -LiteralPath $testRoot -Recurse -Force
    New-Item -ItemType Directory -Path $staging | Out-Null
    New-Item -ItemType Directory -Path $final | Out-Null
    Set-Content (Join-Path $staging 'new.txt') 'new'
    Set-Content (Join-Path $final 'old.txt') 'old'
    try {
        Publish-PortableStaging -StagingDirectory $staging -FinalDirectory $final `
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
        throw 'backup cleanup failure did not leave the backup remainder for inspection'
    }
    if (Test-Path -LiteralPath $staging) {
        throw 'backup cleanup failure left staging after it became final'
    }
} finally {
    Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output 'build_portable tests: PASS'
