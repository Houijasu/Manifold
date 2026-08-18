<#
.SYNOPSIS
    Build and verify a baseline x86-64 Manifold executable.

.DESCRIPTION
    Builds native and portable references in dedicated target directories, requires
    the pinned bench/perft signatures, exercises the force-magic backend, rejects
    BMI2 instructions in the portable disassembly, and publishes verified artifacts
    through a rollback-safe staging directory.
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetRoot = Join-Path $repositoryRoot 'target'
$nativeTarget = Join-Path $targetRoot 'native-build'
$portableBuildTarget = Join-Path $targetRoot 'portable-build'
$portableDirectory = Join-Path $targetRoot 'portable'
$stagingDirectory = Join-Path $targetRoot "portable-staging-$([guid]::NewGuid().ToString('N'))"
$backupDirectory = Join-Path $targetRoot 'portable-backup'
$ordinaryRelease = Join-Path $targetRoot 'release\manifold.exe'
$networkPath = Join-Path $repositoryRoot 'nets\main.nnue'
$nativeRustFlags = '-C target-cpu=native'
$portableRustFlags = '-C target-cpu=x86-64'
$expectedBenchSignature = 37420
$expectedPerftSignature = 4865609
$forbiddenInstructions = @('pext', 'pdep', 'bzhi', 'mulx', 'sarx', 'shlx', 'shrx', 'rorx')

function New-PortableFailure {
    param(
        [string]$Message,
        [int]$ExitCode
    )
    $exception = [System.InvalidOperationException]::new($Message)
    $exception.Data['ExitCode'] = $ExitCode
    return $exception
}

function Assert-NativeSuccess {
    param(
        [int]$NativeExitCode,
        [string]$Description,
        [int]$FailureExitCode
    )
    if ($NativeExitCode -ne 0) {
        throw (New-PortableFailure "ABORT: $Description failed (exit $NativeExitCode)." $FailureExitCode)
    }
}

function Invoke-WithRestoredBuildEnvironment {
    param([scriptblock]$Body)

    $hadCargoTargetDir = Test-Path Env:CARGO_TARGET_DIR
    $savedCargoTargetDir = $env:CARGO_TARGET_DIR
    $hadRustFlags = Test-Path Env:RUSTFLAGS
    $savedRustFlags = $env:RUSTFLAGS
    try {
        & $Body
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
}

function Get-ValidatedHeadCommit {
    $output = @(& git rev-parse HEAD 2>&1)
    $exitCode = $LASTEXITCODE
    Assert-NativeSuccess $exitCode 'git rev-parse HEAD' 5
    if ($output.Count -ne 1 -or [string]$output[0] -notmatch '\A[0-9a-fA-F]{40}\z') {
        throw (New-PortableFailure 'ABORT: git did not return exactly one 40-hex source commit.' 5)
    }
    return [string]$output[0]
}

function Assert-BuildInputsMatchHead {
    $changes = @(& git status --porcelain --untracked-files=normal -- Cargo.toml Cargo.lock .cargo crates 2>&1)
    $exitCode = $LASTEXITCODE
    Assert-NativeSuccess $exitCode 'git status for build inputs' 5
    if ($changes.Count -ne 0) {
        throw (New-PortableFailure "ABORT: build inputs differ from HEAD: $($changes -join '; ')." 5)
    }
}

function Invoke-CargoBuild {
    param(
        [string]$RustFlags,
        [string]$TargetDir
    )
    $env:RUSTFLAGS = $RustFlags
    $env:CARGO_TARGET_DIR = $TargetDir
    $output = @(& cargo build --release -p mf-uci --bin manifold 2>&1)
    $exitCode = $LASTEXITCODE
    $output | Out-Host
    Assert-NativeSuccess $exitCode "cargo build with RUSTFLAGS=$RustFlags" 3
    $executable = Join-Path $TargetDir 'release\manifold.exe'
    if (-not (Test-Path -LiteralPath $executable)) {
        throw (New-PortableFailure "ABORT: cargo build did not produce $executable." 3)
    }
    return $executable
}

function Get-NodesSignatureFromOutput {
    param(
        [string]$Output,
        [string]$Description
    )
    $matches = [regex]::Matches($Output, '(?m)^Nodes searched:\s*(\d+)\s*$')
    if ($matches.Count -ne 1) {
        throw (New-PortableFailure "ABORT: no node signature in $Description output." 5)
    }
    return [int64]$matches[0].Groups[1].Value
}

function Invoke-EngineNodesCommand {
    param(
        [string]$Binary,
        [string[]]$Arguments,
        [string]$Description
    )
    $output = @(& $Binary @Arguments 2>&1)
    $exitCode = $LASTEXITCODE
    $output | Out-Host
    Assert-NativeSuccess $exitCode $Description 5
    return Get-NodesSignatureFromOutput ($output -join [Environment]::NewLine) $Description
}

function Invoke-ForceMagicTests {
    $env:RUSTFLAGS = $portableRustFlags
    $env:CARGO_TARGET_DIR = $portableBuildTarget
    $output = @(& cargo test -p mf-core --features force-magic 2>&1)
    $exitCode = $LASTEXITCODE
    $output | Out-Host
    Assert-NativeSuccess $exitCode 'cargo test -p mf-core --features force-magic' 6
}

function Get-RustcVerboseVersion {
    $output = @(& rustc -vV 2>&1)
    $exitCode = $LASTEXITCODE
    Assert-NativeSuccess $exitCode 'rustc -vV' 5
    return $output -join [Environment]::NewLine
}

function Find-LlvmObjdump {
    $sysrootOutput = @(& rustc --print sysroot 2>&1)
    $exitCode = $LASTEXITCODE
    Assert-NativeSuccess $exitCode 'rustc --print sysroot' 2
    if ($sysrootOutput.Count -ne 1) {
        throw (New-PortableFailure 'ABORT: rustc returned an invalid sysroot.' 2)
    }
    $sysroot = [string]$sysrootOutput[0]
    $profdata = Join-Path $sysroot 'lib\rustlib\x86_64-pc-windows-msvc\bin\llvm-profdata.exe'
    if (-not (Test-Path -LiteralPath $profdata)) {
        throw (New-PortableFailure "ABORT: llvm-profdata.exe is unavailable.`n`nrustup component add llvm-tools-preview" 2)
    }
    $objdump = Join-Path (Split-Path -Parent $profdata) 'llvm-objdump.exe'
    if (-not (Test-Path -LiteralPath $objdump)) {
        throw (New-PortableFailure "ABORT: llvm-objdump.exe is unavailable.`n`nrustup component add llvm-tools-preview" 2)
    }
    return [System.IO.Path]::GetFullPath($objdump)
}

function Get-ForbiddenInstructionTokens {
    param([string[]]$DisassemblyLines)

    foreach ($line in $DisassemblyLines) {
        if ($line -match '^\s*[0-9a-fA-F]+:\s+(?:[0-9a-fA-F]{2}\s+)+([A-Za-z][A-Za-z0-9.]*)\b') {
            $instruction = $Matches[1].ToLowerInvariant()
            if ($forbiddenInstructions -contains $instruction) {
                $instruction
            }
        }
    }
}

function Get-ForbiddenInstructionsInBinary {
    param(
        [string]$Objdump,
        [string]$Binary
    )
    $output = @(& $Objdump --disassemble --x86-asm-syntax=intel $Binary 2>&1)
    $exitCode = $LASTEXITCODE
    Assert-NativeSuccess $exitCode "llvm-objdump on $Binary" 7
    return @(Get-ForbiddenInstructionTokens $output | Sort-Object -Unique)
}

function Assert-StableFileHash {
    param(
        [string]$Path,
        [string]$ExpectedHash,
        [string]$Description
    )
    if (-not (Test-Path -LiteralPath $Path)) {
        throw (New-PortableFailure "ABORT: $Description disappeared: $Path." 8)
    }
    $actualHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($actualHash -ne $ExpectedHash) {
        throw (New-PortableFailure "ABORT: $Description changed ($ExpectedHash -> $actualHash)." 8)
    }
    return $actualHash
}

function Stage-PortableBinary {
    param(
        [string]$BuildOutput,
        [string]$StagingDirectory
    )
    if (Test-Path -LiteralPath $StagingDirectory) {
        throw (New-PortableFailure "ABORT: portable staging path already exists: $StagingDirectory." 8)
    }
    New-Item -ItemType Directory -Path $StagingDirectory | Out-Null
    $stagedBinary = Join-Path $StagingDirectory 'manifold.exe'
    Copy-Item -LiteralPath $BuildOutput -Destination $stagedBinary
    $buildHash = (Get-FileHash -LiteralPath $BuildOutput -Algorithm SHA256).Hash
    Assert-StableFileHash $stagedBinary $buildHash 'staged portable binary' | Out-Null
    return $stagedBinary
}

function Confirm-OrdinaryReleasePreserved {
    param(
        [bool]$ExistedBefore,
        [string]$HashBefore
    )
    $existsAfter = Test-Path -LiteralPath $ordinaryRelease
    if ($existsAfter -ne $ExistedBefore) {
        throw (New-PortableFailure 'ABORT: target\release\manifold.exe existence changed during the portable build.' 7)
    }
    if (-not $ExistedBefore) {
        return 'target\release\manifold.exe was absent before the run and remains absent.'
    }
    $hashAfter = (Get-FileHash -LiteralPath $ordinaryRelease -Algorithm SHA256).Hash
    if ($hashAfter -ne $HashBefore) {
        throw (New-PortableFailure "ABORT: target\release\manifold.exe changed ($HashBefore -> $hashAfter)." 7)
    }
    return "target\release\manifold.exe verified unchanged (sha256 $hashAfter)."
}

function Assert-PortableArtifacts {
    param(
        [string]$Directory,
        [string]$SourceCommit,
        [string]$BinaryHash,
        [string]$NetworkHash
    )
    $binary = Join-Path $Directory 'manifold.exe'
    $sidecar = Join-Path $Directory 'manifold.exe.source-commit'
    $metadataPath = Join-Path $Directory 'build-metadata.txt'
    foreach ($path in @($binary, $sidecar, $metadataPath)) {
        if (-not (Test-Path -LiteralPath $path)) {
            throw (New-PortableFailure "ABORT: portable artifact is missing: $path." 8)
        }
    }
    if ([System.IO.File]::ReadAllText($sidecar) -cne $SourceCommit) {
        throw (New-PortableFailure "ABORT: invalid source sidecar: $sidecar." 8)
    }
    if ((Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash -ne $BinaryHash) {
        throw (New-PortableFailure 'ABORT: portable binary hash changed during publication.' 8)
    }
    $metadata = Get-Content -LiteralPath $metadataPath -Raw
    foreach ($required in @(
        $SourceCommit,
        $BinaryHash,
        $NetworkHash,
        $portableRustFlags,
        [string]$expectedBenchSignature,
        [string]$expectedPerftSignature
    )) {
        if (-not $metadata.Contains($required)) {
            throw (New-PortableFailure "ABORT: portable metadata is missing $required." 8)
        }
    }
}

function Publish-PortableStaging {
    param(
        [string]$StagingDirectory,
        [string]$FinalDirectory,
        [string]$BackupDirectory,
        [scriptblock]$ValidatePublished
    )
    if (Test-Path -LiteralPath $BackupDirectory) {
        throw (New-PortableFailure "ABORT: stale portable publication backup requires inspection: $BackupDirectory." 8)
    }
    $hadFinalDirectory = Test-Path -LiteralPath $FinalDirectory
    $backupMoveSucceeded = $false
    $stagingInstallAttempted = $false
    try {
        if ($hadFinalDirectory) {
            Move-Item -LiteralPath $FinalDirectory -Destination $BackupDirectory
            $backupMoveSucceeded = $true
        }
        $stagingInstallAttempted = $true
        Move-Item -LiteralPath $StagingDirectory -Destination $FinalDirectory
        & $ValidatePublished
        if ($backupMoveSucceeded) {
            Remove-Item -LiteralPath $BackupDirectory -Recurse -Force
            $backupMoveSucceeded = $false
        }
    } catch {
        $publicationFailure = $_
        try {
            if ($backupMoveSucceeded) {
                Remove-Item -LiteralPath $FinalDirectory -Recurse -Force -ErrorAction SilentlyContinue
                Move-Item -LiteralPath $BackupDirectory -Destination $FinalDirectory
                $backupMoveSucceeded = $false
            } elseif (-not $hadFinalDirectory -and $stagingInstallAttempted) {
                Remove-Item -LiteralPath $FinalDirectory -Recurse -Force -ErrorAction SilentlyContinue
            }
        } catch {
            throw (New-PortableFailure "ABORT: portable publication failed and rollback also failed: $($_.Exception.Message)" 8)
        }
        throw $publicationFailure
    } finally {
        Remove-Item -LiteralPath $StagingDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-PortableBuild {
    Push-Location $repositoryRoot
    try {
        $sourceCommit = Get-ValidatedHeadCommit
        Assert-BuildInputsMatchHead
        if (-not (Test-Path -LiteralPath $networkPath)) {
            throw (New-PortableFailure "ABORT: required embedded network is missing: $networkPath." 3)
        }
        $objdump = Find-LlvmObjdump
        $networkHash = (Get-FileHash -LiteralPath $networkPath -Algorithm SHA256).Hash
        $releaseExistedBefore = Test-Path -LiteralPath $ordinaryRelease
        $releaseHashBefore = if ($releaseExistedBefore) {
            (Get-FileHash -LiteralPath $ordinaryRelease -Algorithm SHA256).Hash
        } else {
            ''
        }

        if (Test-Path -LiteralPath $backupDirectory) {
            throw (New-PortableFailure "ABORT: stale portable publication backup requires inspection: $backupDirectory." 8)
        }
        try {
            Write-Host '== Stage 1/6: native reference build and bench =='
            $nativeExe = Invoke-CargoBuild $nativeRustFlags $nativeTarget
            $nativeBench = Invoke-EngineNodesCommand $nativeExe @('bench') 'native bench'
            if ($nativeBench -ne $expectedBenchSignature) {
                throw (New-PortableFailure "ABORT: native bench signature is $nativeBench, expected $expectedBenchSignature." 4)
            }

            Write-Host '== Stage 2/6: portable x86-64 build and bench =='
            $portableExe = Invoke-CargoBuild $portableRustFlags $portableBuildTarget
            $stagedBinary = Stage-PortableBinary $portableExe $stagingDirectory
            $binaryHash = (Get-FileHash -LiteralPath $stagedBinary -Algorithm SHA256).Hash
            Assert-StableFileHash $networkPath $networkHash 'embedded network' | Out-Null
            $portableBench = Invoke-EngineNodesCommand $stagedBinary @('bench') 'portable bench'
            if ($portableBench -ne $expectedBenchSignature) {
                throw (New-PortableFailure "ABORT: portable bench signature is $portableBench, expected $expectedBenchSignature." 4)
            }

            Write-Host '== Stage 3/6: portable perft =='
            $portablePerft = Invoke-EngineNodesCommand $stagedBinary @('perft', '5') 'portable perft 5'
            if ($portablePerft -ne $expectedPerftSignature) {
                throw (New-PortableFailure "ABORT: portable perft 5 is $portablePerft, expected $expectedPerftSignature." 4)
            }

            Write-Host '== Stage 4/6: force-magic tests =='
            Invoke-ForceMagicTests

            Write-Host '== Stage 5/6: instruction scan =='
            $nativeForbidden = @(Get-ForbiddenInstructionsInBinary $objdump $nativeExe)
            $portableForbidden = @(Get-ForbiddenInstructionsInBinary $objdump $stagedBinary)
            if ($portableForbidden.Count -ne 0) {
                throw (New-PortableFailure "ABORT: portable binary contains forbidden instructions: $($portableForbidden -join ', ')." 7)
            }
            $nativeScan = if ($nativeForbidden.Count -eq 0) { 'none' } else { $nativeForbidden -join ', ' }
            Write-Host "native forbidden instructions (informational): $nativeScan"
            Write-Host 'portable forbidden instructions: none'

            Write-Host '== Stage 6/6: metadata and failure-safe publication =='
            Assert-BuildInputsMatchHead
            if ((Get-ValidatedHeadCommit) -cne $sourceCommit) {
                throw (New-PortableFailure 'ABORT: HEAD changed during the portable build.' 5)
            }
            Assert-StableFileHash $networkPath $networkHash 'embedded network' | Out-Null
            Assert-StableFileHash $stagedBinary $binaryHash 'staged portable binary' | Out-Null
            Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore | Out-Null

            [System.IO.File]::WriteAllText((Join-Path $stagingDirectory 'manifold.exe.source-commit'), $sourceCommit)
            $rustcVersion = Get-RustcVerboseVersion
            $releaseStatus = Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore
            Assert-StableFileHash $networkPath $networkHash 'embedded network' | Out-Null
            Assert-StableFileHash $stagedBinary $binaryHash 'staged portable binary' | Out-Null
            @(
                "date: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss K')"
                "engine source HEAD: $sourceCommit"
                "rustc -vV:"
                $rustcVersion
                "native RUSTFLAGS: $nativeRustFlags"
                "portable RUSTFLAGS: $portableRustFlags"
                "portable CARGO_TARGET_DIR: $portableBuildTarget"
                "network: nets\main.nnue"
                "network sha256: $networkHash"
                "binary: target\portable\manifold.exe"
                "binary sha256: $binaryHash"
                "native bench signature: $nativeBench"
                "portable bench signature: $portableBench"
                "portable perft 5: $portablePerft"
                "force-magic tests: passed"
                "disassembler: $objdump"
                "native forbidden instructions (informational): $nativeScan"
                "portable forbidden instructions: none"
                "ordinary release: $releaseStatus"
            ) | Set-Content -LiteralPath (Join-Path $stagingDirectory 'build-metadata.txt')

            Assert-PortableArtifacts $stagingDirectory $sourceCommit $binaryHash $networkHash
            if ((Get-ValidatedHeadCommit) -cne $sourceCommit) {
                throw (New-PortableFailure 'ABORT: HEAD changed immediately before portable publication.' 5)
            }
            Assert-StableFileHash $networkPath $networkHash 'embedded network' | Out-Null
            Assert-StableFileHash $stagedBinary $binaryHash 'staged portable binary' | Out-Null
            Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore | Out-Null
            Publish-PortableStaging $stagingDirectory $portableDirectory $backupDirectory {
                Assert-PortableArtifacts $portableDirectory $sourceCommit $binaryHash $networkHash
                Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore | Out-Null
            }
            $releaseStatus = Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore
            Write-Host "Portable artifact complete: $(Join-Path $portableDirectory 'manifold.exe')"
            Write-Host $releaseStatus
        } finally {
            Remove-Item -LiteralPath $stagingDirectory -Recurse -Force -ErrorAction SilentlyContinue
        }
    } finally {
        Pop-Location
    }
}

if ($MyInvocation.InvocationName -ne '.') {
    try {
        Invoke-WithRestoredBuildEnvironment { Invoke-PortableBuild }
    } catch {
        Write-Host $_.Exception.Message
        $exitCode = $_.Exception.Data['ExitCode']
        if ($exitCode -isnot [int]) { $exitCode = 1 }
        exit $exitCode
    }
}
