<#
.SYNOPSIS
    Reproducible PGO (profile-guided optimisation) build for Manifold.

.DESCRIPTION
    research/rust-perf-and-nnue-training.md section 0.5 verified the full
    -Cprofile-generate / -Cprofile-use round-trip on this MSVC toolchain. This driver
    encodes it so the profile cannot silently rot against the source:

      STAGE 1  Plain release build in target\pgo-build\baseline; its `bench` node
               signature is recorded as the reference.
      STAGE 2  Instrumented build (-Cprofile-generate) and `-BenchRuns` `manifold
               bench` invocations. bench is the representative workload: deterministic,
               covers search + eval, and is one command.
      STAGE 3  llvm-profdata merge (found inside the rust-toolchain.toml-pinned
               toolchain; refuses to run with exit 2 if llvm-tools-preview is absent).
      STAGE 4  Optimised build with -Cprofile-use in target\pgo-build\optimized.
      STAGE 5  Verification gate: the PGO binary's `bench` node signature MUST equal
               the stage-1 reference. A drift means the optimiser changed the search,
               which is a defect, not a speedup -- abort with exit 4. Both copies,
               sidecars, hashes, and metadata are validated in staging before
               target\pgo is replaced.

    IMPORTANT: setting $env:RUSTFLAGS REPLACES the rustflags in .cargo/config.toml
    wholesale, so every stage passes `-C target-cpu=native` explicitly. Dropping it
    would silently lose BMI2/PEXT and the AVX-VNNI NNUE kernels and make any
    before/after comparison meaningless.

    Measured on this repo (2026-08, depth-12 nps_compare medians): geomean 1.00x --
    parity, because fat LTO + codegen-units=1 + target-cpu=native leave PGO little
    headroom. Revisit after large source changes; a stale profile is worse than none
    only conceptually, LLVM ignores counts that no longer map onto the IR.

.EXAMPLE
    # Full pipeline: instrument, profile from 3 bench runs, merge, optimise, verify.
    .\harness\build_pgo.ps1

.EXAMPLE
    # More profiling coverage and an explicit NPS verdict against the baseline copy.
    .\harness\build_pgo.ps1 -BenchRuns 5 -MeasureNps
#>
[CmdletBinding()]
param(
    # Number of `manifold bench` profile runs between instrumentation and merge.
    [ValidateRange(1, 100)]
    [int]$BenchRuns = 3,

    # After stage 5, run harness\nps_compare.py (nopgo vs pgo) for a measured verdict.
    [switch]$MeasureNps
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$targetRoot = Join-Path $repositoryRoot 'target'
$baselineTarget = Join-Path $targetRoot 'pgo-build\baseline'
$instrumentedTarget = Join-Path $targetRoot 'pgo-build\instrumented'
$optimizedTarget = Join-Path $targetRoot 'pgo-build\optimized'
$pgoDirectory = Join-Path $targetRoot 'pgo'
$stagingDirectory = Join-Path $targetRoot 'pgo-staging'
$backupDirectory = Join-Path $targetRoot 'pgo-backup'
$ordinaryRelease = Join-Path $targetRoot 'release\manifold.exe'

function New-PgoFailure {
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
        throw (New-PgoFailure "ABORT: $Description failed (exit $NativeExitCode)." $FailureExitCode)
    }
}

function Assert-ExactPath {
    param(
        [string]$ActualPath,
        [string]$ExpectedPath,
        [string]$Description
    )
    $actual = [System.IO.Path]::GetFullPath($ActualPath).TrimEnd('\', '/')
    $expected = [System.IO.Path]::GetFullPath($ExpectedPath).TrimEnd('\', '/')
    if (-not $actual.Equals($expected, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw (New-PgoFailure "ABORT: $Description must resolve exactly to $expected, got $actual." 2)
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

function Invoke-CargoBuild {
    param(
        [string]$RustFlags,
        [string]$TargetDir
    )
    if ($RustFlags) { $env:RUSTFLAGS = $RustFlags } else { Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue }
    $env:CARGO_TARGET_DIR = $TargetDir
    & cargo build --release -p mf-uci | Out-Host
    $cargoExitCode = $LASTEXITCODE
    Assert-NativeSuccess $cargoExitCode 'cargo build' 3
    $executable = Join-Path $TargetDir 'release\manifold.exe'
    if (-not (Test-Path -LiteralPath $executable)) {
        throw (New-PgoFailure "ABORT: cargo build did not produce $executable." 3)
    }
    return $executable
}

function Get-BenchSignature {
    param([string]$Binary)
    $out = & $Binary bench 2>&1 | Out-String
    $benchExitCode = $LASTEXITCODE
    Assert-NativeSuccess $benchExitCode "bench on $Binary" 5
    if ($out -notmatch 'Nodes searched:\s*(\d+)') {
        throw (New-PgoFailure "ABORT: no node signature in bench output of $Binary." 5)
    }
    return [int64]$Matches[1]
}

function Find-LlvmProfdata {
    $toml = Get-Content 'rust-toolchain.toml' -Raw
    $channels = @()
    if ($toml -match 'channel\s*=\s*"([^"]+)"') { $channels += $Matches[1] }
    $channels += 'stable'
    foreach ($channel in $channels) {
        $candidate = Join-Path $env:USERPROFILE ".rustup\toolchains\$channel-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\llvm-profdata.exe"
        if (Test-Path $candidate) { return $candidate }
    }
    Write-Host 'ABORT: llvm-profdata.exe not found in the pinned or stable toolchain.'
    Write-Host '       Run: rustup component add llvm-tools-preview'
    throw (New-PgoFailure 'ABORT: llvm-profdata.exe is unavailable.' 2)
}

function Get-ValidatedHeadCommit {
    $output = @(& git rev-parse HEAD 2>&1)
    $gitExitCode = $LASTEXITCODE
    Assert-NativeSuccess $gitExitCode 'git rev-parse HEAD' 5
    if ($output.Count -ne 1 -or [string]$output[0] -notmatch '\A[0-9a-fA-F]{40}\z') {
        throw (New-PgoFailure 'ABORT: git did not return exactly one 40-hex source commit.' 5)
    }
    return [string]$output[0]
}

function Get-WorkingTreeState {
    $lines = @(& git status --porcelain --untracked-files=normal 2>&1)
    $gitExitCode = $LASTEXITCODE
    Assert-NativeSuccess $gitExitCode 'git status' 5
    return [pscustomobject]@{
        State = if ($lines.Count -eq 0) { 'clean' } else { 'dirty' }
        Details = if ($lines.Count -eq 0) { '(none)' } else { $lines -join '; ' }
    }
}

function Assert-PgoArtifacts {
    param(
        [string]$Directory,
        [string]$SourceCommit,
        [string]$BaselineHash,
        [string]$OptimizedHash,
        [string]$ProfileHash
    )

    $baseline = Join-Path $Directory 'manifold-nopgo.exe'
    $optimized = Join-Path $Directory 'manifold-pgo.exe'
    $metadataPath = Join-Path $Directory 'pgo-metadata.txt'
    foreach ($path in @($baseline, $optimized, "$baseline.source-commit", "$optimized.source-commit", $metadataPath, (Join-Path $Directory 'merged.profdata'))) {
        if (-not (Test-Path -LiteralPath $path)) {
            throw (New-PgoFailure "ABORT: staged PGO artifact is missing: $path." 5)
        }
    }
    foreach ($sidecar in @("$baseline.source-commit", "$optimized.source-commit")) {
        if ([System.IO.File]::ReadAllText($sidecar) -cne $SourceCommit) {
            throw (New-PgoFailure "ABORT: invalid source sidecar: $sidecar." 5)
        }
    }
    if ((Get-FileHash $baseline -Algorithm SHA256).Hash -ne $BaselineHash) {
        throw (New-PgoFailure 'ABORT: baseline hash changed during publication.' 5)
    }
    if ((Get-FileHash $optimized -Algorithm SHA256).Hash -ne $OptimizedHash) {
        throw (New-PgoFailure 'ABORT: optimized hash changed during publication.' 5)
    }
    if ((Get-FileHash (Join-Path $Directory 'merged.profdata') -Algorithm SHA256).Hash -ne $ProfileHash) {
        throw (New-PgoFailure 'ABORT: profile hash changed during publication.' 5)
    }
    $metadata = Get-Content -LiteralPath $metadataPath -Raw
    foreach ($required in @($SourceCommit, $BaselineHash, $OptimizedHash, $ProfileHash)) {
        if (-not $metadata.Contains($required)) {
            throw (New-PgoFailure "ABORT: PGO metadata is missing $required." 5)
        }
    }
}

function Publish-PgoStaging {
    param(
        [string]$StagingDirectory,
        [string]$FinalDirectory,
        [string]$BackupDirectory,
        [scriptblock]$ValidatePublished
    )

    if (Test-Path -LiteralPath $BackupDirectory) {
        throw (New-PgoFailure "ABORT: stale PGO publication backup requires inspection: $BackupDirectory." 8)
    }
    $hadFinalDirectory = Test-Path -LiteralPath $FinalDirectory
    try {
        if ($hadFinalDirectory) {
            Move-Item -LiteralPath $FinalDirectory -Destination $BackupDirectory
        }
        Move-Item -LiteralPath $StagingDirectory -Destination $FinalDirectory
        & $ValidatePublished
        if ($hadFinalDirectory) {
            Remove-Item -LiteralPath $BackupDirectory -Recurse -Force
        }
    } catch {
        $publicationFailure = $_
        try {
            Remove-Item -LiteralPath $FinalDirectory -Recurse -Force -ErrorAction SilentlyContinue
            if ($hadFinalDirectory -and (Test-Path -LiteralPath $BackupDirectory)) {
                Move-Item -LiteralPath $BackupDirectory -Destination $FinalDirectory
            }
        } catch {
            throw (New-PgoFailure "ABORT: PGO publication failed and rollback also failed: $($_.Exception.Message)" 8)
        }
        throw $publicationFailure
    } finally {
        Remove-Item -LiteralPath $StagingDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Confirm-OrdinaryReleasePreserved {
    param(
        [bool]$ExistedBefore,
        [string]$HashBefore
    )

    $existsAfter = Test-Path -LiteralPath $ordinaryRelease
    if ($existsAfter -ne $ExistedBefore) {
        throw (New-PgoFailure 'ABORT: target\release\manifold.exe existence changed during the PGO run.' 7)
    }
    if (-not $ExistedBefore) {
        return 'target\release\manifold.exe was absent before the run and remains absent.'
    }
    $hashAfter = (Get-FileHash $ordinaryRelease -Algorithm SHA256).Hash
    if ($hashAfter -ne $HashBefore) {
        throw (New-PgoFailure "ABORT: target\release\manifold.exe changed ($HashBefore -> $hashAfter)." 7)
    }
    return "target\release\manifold.exe verified unchanged (sha256 $hashAfter)."
}

function Invoke-PgoBuild {
    Push-Location $repositoryRoot
    try {
        Assert-ExactPath $pgoDirectory (Join-Path $repositoryRoot 'target\pgo') 'PGO publication directory'
        $sourceCommit = Get-ValidatedHeadCommit
        $workingTreeAtStart = Get-WorkingTreeState
        $releaseExistedBefore = Test-Path -LiteralPath $ordinaryRelease
        $releaseHashBefore = if ($releaseExistedBefore) {
            (Get-FileHash $ordinaryRelease -Algorithm SHA256).Hash
        } else {
            ''
        }

        if (Test-Path -LiteralPath $backupDirectory) {
            throw (New-PgoFailure "ABORT: stale PGO publication backup requires inspection: $backupDirectory." 8)
        }
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force -ErrorAction SilentlyContinue
        New-Item -ItemType Directory -Path $stagingDirectory | Out-Null

        try {
            Write-Host '== Stage 1/5: baseline build and reference signature =='
            $baselineExe = Invoke-CargoBuild '' $baselineTarget
            $nodesBefore = Get-BenchSignature $baselineExe
            Write-Host "reference signature: $nodesBefore nodes"

            Write-Host '== Stage 2/5: instrumented build and profiling runs =='
            $instrumentedExe = Invoke-CargoBuild "-C target-cpu=native -Cprofile-generate=$stagingDirectory" $instrumentedTarget
            1..$BenchRuns | ForEach-Object {
                & $instrumentedExe bench | Out-Null
                $profileExitCode = $LASTEXITCODE
                Assert-NativeSuccess $profileExitCode "profiling run $_" 5
            }
            $profraws = @(Get-ChildItem $stagingDirectory -Filter '*.profraw')
            if ($profraws.Count -eq 0) {
                throw (New-PgoFailure "ABORT: no .profraw produced in $stagingDirectory." 5)
            }
            Write-Host "collected $($profraws.Count) profraw file(s)"

            Write-Host '== Stage 3/5: merge profiles =='
            $profdata = Find-LlvmProfdata
            $mergedProfile = Join-Path $stagingDirectory 'merged.profdata'
            & $profdata merge -o $mergedProfile ($profraws.FullName)
            $mergeExitCode = $LASTEXITCODE
            Assert-NativeSuccess $mergeExitCode 'llvm-profdata merge' 3

            Write-Host '== Stage 4/5: PGO-optimised build =='
            Remove-Item -LiteralPath $optimizedTarget -Recurse -Force -ErrorAction SilentlyContinue
            $optimizedExe = Invoke-CargoBuild "-C target-cpu=native -Cprofile-use=$mergedProfile" $optimizedTarget

            Write-Host '== Stage 5/5: node-signature verification =='
            $nodesAfter = Get-BenchSignature $optimizedExe
            if ($nodesAfter -ne $nodesBefore) {
                throw (New-PgoFailure "ABORT: node signature drifted ($nodesBefore -> $nodesAfter). The optimiser changed the search." 4)
            }
            Write-Host "signature verified: $nodesAfter nodes (unchanged)"

            $headBeforePublication = Get-ValidatedHeadCommit
            if ($headBeforePublication -cne $sourceCommit) {
                throw (New-PgoFailure "ABORT: HEAD changed during the PGO run ($sourceCommit -> $headBeforePublication)." 5)
            }

            $stagedBaseline = Join-Path $stagingDirectory 'manifold-nopgo.exe'
            $stagedOptimized = Join-Path $stagingDirectory 'manifold-pgo.exe'
            Copy-Item -LiteralPath $baselineExe -Destination $stagedBaseline
            Copy-Item -LiteralPath $optimizedExe -Destination $stagedOptimized
            [System.IO.File]::WriteAllText("$stagedBaseline.source-commit", $sourceCommit)
            [System.IO.File]::WriteAllText("$stagedOptimized.source-commit", $sourceCommit)

            $baselineHash = (Get-FileHash $stagedBaseline -Algorithm SHA256).Hash
            $optimizedHash = (Get-FileHash $stagedOptimized -Algorithm SHA256).Hash
            $profileHash = (Get-FileHash $mergedProfile -Algorithm SHA256).Hash
            $rustcOutput = @(& rustc --version 2>&1)
            $rustcExitCode = $LASTEXITCODE
            Assert-NativeSuccess $rustcExitCode 'rustc --version' 5
            $workingTreeBeforePublication = Get-WorkingTreeState
            @(
                "date:         $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
                "commit:       $sourceCommit"
                "tree start:   $($workingTreeAtStart.State)"
                "changes start:$($workingTreeAtStart.Details)"
                "tree publish: $($workingTreeBeforePublication.State)"
                "changes pub:  $($workingTreeBeforePublication.Details)"
                "rustc:        $($rustcOutput -join ' ')"
                "baseline:     target\pgo\manifold-nopgo.exe (sha256 $baselineHash)"
                "optimized:    target\pgo\manifold-pgo.exe (sha256 $optimizedHash)"
                "profile:      target\pgo\merged.profdata (sha256 $profileHash)"
                "runs:         $BenchRuns x manifold bench"
                "signature:    $nodesAfter nodes (verified unchanged)"
            ) | Set-Content (Join-Path $stagingDirectory 'pgo-metadata.txt')

            Assert-PgoArtifacts $stagingDirectory $sourceCommit $baselineHash $optimizedHash $profileHash
            if ((Get-ValidatedHeadCommit) -cne $sourceCommit) {
                throw (New-PgoFailure 'ABORT: HEAD changed immediately before PGO publication.' 5)
            }
            Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore | Out-Null
            $publishedBaseline = Join-Path $pgoDirectory 'manifold-nopgo.exe'
            $publishedOptimized = Join-Path $pgoDirectory 'manifold-pgo.exe'
            Publish-PgoStaging $stagingDirectory $pgoDirectory $backupDirectory {
                Assert-PgoArtifacts $pgoDirectory $sourceCommit $baselineHash $optimizedHash $profileHash
                if ($MeasureNps) {
                    Write-Host '== NPS comparison (baseline vs PGO) =='
                    & py -3.14 harness\nps_compare.py --engine "A=$publishedBaseline" --engine "B=$publishedOptimized" --depth 12 --hash 64 --warmup 1 --repeat 3
                    $npsExitCode = $LASTEXITCODE
                    Assert-NativeSuccess $npsExitCode 'NPS comparison' 6
                }
                Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore | Out-Null
            }

            $releaseStatus = Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore
            Write-Host "Experimental PGO outputs complete: $publishedOptimized (baseline at $publishedBaseline)."
            Write-Host 'These are experiment artifacts, not shipping/release artifacts.'
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
        Invoke-WithRestoredBuildEnvironment { Invoke-PgoBuild }
    } catch {
        Write-Host $_.Exception.Message
        $exitCode = $_.Exception.Data['ExitCode']
        if ($exitCode -isnot [int]) { $exitCode = 1 }
        exit $exitCode
    }
}
