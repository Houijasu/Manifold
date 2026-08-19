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
$networkPath = Join-Path $repositoryRoot 'nets\main.nnue'

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
    $hadEncodedRustFlags = Test-Path Env:CARGO_ENCODED_RUSTFLAGS
    $savedEncodedRustFlags = $env:CARGO_ENCODED_RUSTFLAGS
    $hadRustupToolchain = Test-Path Env:RUSTUP_TOOLCHAIN
    $savedRustupToolchain = $env:RUSTUP_TOOLCHAIN
    $hadNnueTestNet = Test-Path Env:MF_NNUE_TEST_NET
    $savedNnueTestNet = $env:MF_NNUE_TEST_NET
    try {
        Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
        Remove-Item Env:RUSTUP_TOOLCHAIN -ErrorAction SilentlyContinue
        Remove-Item Env:MF_NNUE_TEST_NET -ErrorAction SilentlyContinue
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
        if ($hadEncodedRustFlags) {
            $env:CARGO_ENCODED_RUSTFLAGS = $savedEncodedRustFlags
        } else {
            Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
        }
        if ($hadRustupToolchain) {
            $env:RUSTUP_TOOLCHAIN = $savedRustupToolchain
        } else {
            Remove-Item Env:RUSTUP_TOOLCHAIN -ErrorAction SilentlyContinue
        }
        if ($hadNnueTestNet) {
            $env:MF_NNUE_TEST_NET = $savedNnueTestNet
        } else {
            Remove-Item Env:MF_NNUE_TEST_NET -ErrorAction SilentlyContinue
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

function Get-ActiveRustToolchainIdentity {
    $sysrootOutput = @(& rustc --print sysroot 2>&1)
    $sysrootExitCode = $LASTEXITCODE
    Assert-NativeSuccess $sysrootExitCode 'rustc --print sysroot' 2
    if ($sysrootOutput.Count -ne 1) {
        throw (New-PgoFailure 'ABORT: rustc returned an invalid sysroot.' 2)
    }

    $verboseOutput = @(& rustc -vV 2>&1)
    $verboseExitCode = $LASTEXITCODE
    Assert-NativeSuccess $verboseExitCode 'rustc -vV' 2
    $hostLine = $verboseOutput | Where-Object { $_ -match '^host:\s*(\S+)$' } | Select-Object -First 1
    if (-not $hostLine -or $hostLine -notmatch '^host:\s*(\S+)$') {
        throw (New-PgoFailure 'ABORT: rustc -vV did not report a host triple.' 2)
    }
    [pscustomobject]@{
        Sysroot = [string]$sysrootOutput[0]
        Host = [string]$Matches[1]
        RustcVerbose = $verboseOutput -join [Environment]::NewLine
    }
}

function Find-LlvmProfdata {
    param([psobject]$Toolchain)

    $candidate = Join-Path $Toolchain.Sysroot "lib\rustlib\$($Toolchain.Host)\bin\llvm-profdata.exe"
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw (New-PgoFailure "ABORT: exact pinned llvm-profdata.exe is unavailable at $candidate.`n       Run: rustup component add llvm-tools-preview" 2)
    }
    return $candidate
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

function Assert-BuildInputsMatchHead {
    param(
        [scriptblock]$GetStatus = {
            param([string[]]$Paths)
            $lines = @(& git status --porcelain --untracked-files=normal -- $Paths 2>&1)
            [pscustomobject]@{ ExitCode = $LASTEXITCODE; Lines = $lines }
        }
    )

    $paths = @('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', '.cargo', 'crates')
    $status = & $GetStatus $paths
    Assert-NativeSuccess $status.ExitCode 'git status for build inputs' 5
    if ($status.Lines.Count -ne 0) {
        throw (New-PgoFailure "ABORT: build inputs differ from HEAD: $($status.Lines -join '; ')." 5)
    }
}

function Get-RequiredFileIdentity {
    param(
        [string]$Path,
        [string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw (New-PgoFailure "ABORT: required $Description is missing: $Path." 3)
    }
    $item = Get-Item -LiteralPath $Path
    [pscustomobject]@{
        Size = $item.Length
        Hash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    }
}

function Assert-StableFileIdentity {
    param(
        [string]$Path,
        [int64]$ExpectedSize,
        [string]$ExpectedHash,
        [string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw (New-PgoFailure "ABORT: $Description changed or disappeared during the PGO run: $Path." 5)
    }
    $item = Get-Item -LiteralPath $Path
    $actualHash = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($item.Length -ne $ExpectedSize -or $actualHash -ne $ExpectedHash) {
        throw (New-PgoFailure "ABORT: $Description changed during the PGO run (size $ExpectedSize -> $($item.Length), sha256 $ExpectedHash -> $actualHash)." 5)
    }
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

function Set-MetadataValue {
    param(
        [string]$MetadataPath,
        [string]$Label,
        [string]$Value
    )

    $lines = @(Get-Content -LiteralPath $MetadataPath)
    $prefix = "$Label`:"
    $replaced = $false
    $updated = foreach ($line in $lines) {
        if ($line.StartsWith($prefix, [System.StringComparison]::Ordinal)) {
            $replaced = $true
            "$prefix $Value"
        } else {
            $line
        }
    }
    if (-not $replaced) {
        throw (New-PgoFailure "ABORT: PGO metadata is missing label $Label." 5)
    }
    Set-Content -LiteralPath $MetadataPath -Value $updated
}

function Write-NpsNotMeasured {
    param(
        [string]$MetadataPath,
        [string]$EvidencePath
    )

    @(
        'status: not measured'
        'command: not run'
        'exit code: not applicable'
        'output:'
        '(none)'
    ) | Set-Content -LiteralPath $EvidencePath
    Set-MetadataValue $MetadataPath 'nps verdict' 'not measured'
    Set-MetadataValue $MetadataPath 'nps evidence' "target\pgo\nps-verdict.txt (sha256 $((Get-FileHash $EvidencePath -Algorithm SHA256).Hash))"
}

function Write-NpsPending {
    param(
        [string]$MetadataPath,
        [string]$EvidencePath
    )

    @(
        'status: pending'
        'command: pending publication'
        'exit code: pending'
        'output:'
        '(pending)'
    ) | Set-Content -LiteralPath $EvidencePath
    Set-MetadataValue $MetadataPath 'nps verdict' 'pending publication'
    Set-MetadataValue $MetadataPath 'nps evidence' "target\pgo\nps-verdict.txt (sha256 $((Get-FileHash $EvidencePath -Algorithm SHA256).Hash))"
}

function Invoke-NpsComparison {
    param(
        [string]$BaselineBinary,
        [string]$OptimizedBinary,
        [string]$MetadataPath,
        [string]$EvidencePath,
        [scriptblock]$RunComparison = {
            param([string]$Baseline, [string]$Optimized)
            $arguments = @(
                '-3.14'
                'harness\nps_compare.py'
                '--engine', "A=$Baseline"
                '--engine', "B=$Optimized"
                '--depth', '12'
                '--hash', '64'
                '--warmup', '1'
                '--repeat', '3'
            )
            $output = @(& py @arguments 2>&1)
            [pscustomobject]@{
                Command = "py $($arguments -join ' ')"
                Output = $output
                ExitCode = $LASTEXITCODE
            }
        }
    )

    $callerPreference = Get-Variable -Name PSNativeCommandUseErrorActionPreference -Scope 1 -ErrorAction SilentlyContinue
    $savedCallerPreference = if ($callerPreference) { $callerPreference.Value } else { $null }
    try {
        Set-Variable -Name PSNativeCommandUseErrorActionPreference -Value $false -Scope 1
        $result = & $RunComparison $BaselineBinary $OptimizedBinary
    } finally {
        if ($callerPreference) {
            Set-Variable -Name PSNativeCommandUseErrorActionPreference -Value $savedCallerPreference -Scope 1
        } else {
            Remove-Variable -Name PSNativeCommandUseErrorActionPreference -Scope 1 -ErrorAction SilentlyContinue
        }
    }
    $status = if ($result.ExitCode -eq 0) { 'passed' } else { 'failed' }
    @(
        "status: $status"
        "command: $($result.Command)"
        "exit code: $($result.ExitCode)"
        'output:'
        $result.Output
    ) | Set-Content -LiteralPath $EvidencePath
    Set-MetadataValue $MetadataPath 'nps verdict' "$status (exit $($result.ExitCode))"
    Set-MetadataValue $MetadataPath 'nps evidence' "target\pgo\nps-verdict.txt (sha256 $((Get-FileHash $EvidencePath -Algorithm SHA256).Hash))"
    $result.Output | Out-Host
    Assert-NativeSuccess $result.ExitCode 'NPS comparison' 6
}

function Assert-PgoArtifacts {
    param(
        [string]$Directory,
        [string]$SourceCommit,
        [string]$BaselineHash,
        [string]$OptimizedHash,
        [string]$ProfileHash,
        [string]$NetworkPath,
        [int64]$NetworkSize,
        [string]$NetworkHash
    )

    Assert-StableFileIdentity $NetworkPath $NetworkSize $NetworkHash 'embedded network'
    $baseline = Join-Path $Directory 'manifold-nopgo.exe'
    $optimized = Join-Path $Directory 'manifold-pgo.exe'
    $metadataPath = Join-Path $Directory 'pgo-metadata.txt'
    $npsEvidencePath = Join-Path $Directory 'nps-verdict.txt'
    foreach ($path in @($baseline, $optimized, "$baseline.source-commit", "$optimized.source-commit", $metadataPath, $npsEvidencePath, (Join-Path $Directory 'merged.profdata'))) {
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
    foreach ($required in @($SourceCommit, $BaselineHash, $OptimizedHash, $ProfileHash, 'network:      nets\main.nnue', "network size: $NetworkSize", "network sha256: $NetworkHash")) {
        if (-not $metadata.Contains($required)) {
            throw (New-PgoFailure "ABORT: PGO metadata is missing $required." 5)
        }
    }
    $npsEvidenceHash = (Get-FileHash $npsEvidencePath -Algorithm SHA256).Hash
    if (-not $metadata.Contains('nps verdict:') -or -not $metadata.Contains($npsEvidenceHash)) {
        throw (New-PgoFailure 'ABORT: PGO metadata has no truthful NPS verdict evidence.' 5)
    }
    Assert-StableFileIdentity $NetworkPath $NetworkSize $NetworkHash 'embedded network'
}

function Publish-PgoStaging {
    param(
        [string]$StagingDirectory,
        [string]$FinalDirectory,
        [string]$BackupDirectory,
        [scriptblock]$ValidatePublished,
        [scriptblock]$MovePath = {
            param([string]$LiteralPath, [string]$Destination)
            Move-Item -LiteralPath $LiteralPath -Destination $Destination
        },
        [scriptblock]$RemoveBackup = {
            param([string]$LiteralPath)
            Remove-Item -LiteralPath $LiteralPath -Recurse -Force
        }
    )

    if (Test-Path -LiteralPath $BackupDirectory) {
        throw (New-PgoFailure "ABORT: stale PGO publication backup requires inspection: $BackupDirectory." 8)
    }
    $hadFinalDirectory = Test-Path -LiteralPath $FinalDirectory
    $backupMoveSucceeded = $false
    $stagingInstallAttempted = $false
    $publicationCommitted = $false
    try {
        if ($hadFinalDirectory) {
            & $MovePath $FinalDirectory $BackupDirectory
            $backupMoveSucceeded = $true
        }
        $stagingInstallAttempted = $true
        & $MovePath $StagingDirectory $FinalDirectory
        & $ValidatePublished
        $publicationCommitted = $true
        if ($backupMoveSucceeded) {
            try {
                & $RemoveBackup $BackupDirectory
            } catch {
                throw (New-PgoFailure "ABORT: PGO publication committed, but backup cleanup failed. The validated final was preserved; backup remainder requires inspection at $BackupDirectory. $($_.Exception.Message)" 8)
            }
            $backupMoveSucceeded = $false
        }
    } catch {
        $publicationFailure = $_
        if ($publicationCommitted) {
            throw $publicationFailure
        }
        try {
            if ($backupMoveSucceeded) {
                Remove-Item -LiteralPath $FinalDirectory -Recurse -Force -ErrorAction SilentlyContinue
                & $MovePath $BackupDirectory $FinalDirectory
                $backupMoveSucceeded = $false
            } elseif (-not $hadFinalDirectory -and $stagingInstallAttempted) {
                Remove-Item -LiteralPath $FinalDirectory -Recurse -Force -ErrorAction SilentlyContinue
            }
        } catch {
            throw (New-PgoFailure "ABORT: PGO publication failed and rollback also failed: $($_.Exception.Message)" 8)
        }
        throw $publicationFailure
    } finally {
        Remove-Item -LiteralPath $StagingDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Publish-AndMeasurePgo {
    param(
        [string]$StagingDirectory,
        [string]$FinalDirectory,
        [string]$BackupDirectory,
        [scriptblock]$ValidatePublished,
        [switch]$MeasureNps,
        [scriptblock]$RunComparison
    )

    Publish-PgoStaging $StagingDirectory $FinalDirectory $BackupDirectory $ValidatePublished
    if ($MeasureNps) {
        Write-Host '== NPS comparison (baseline vs PGO) =='
        $arguments = @{
            BaselineBinary = Join-Path $FinalDirectory 'manifold-nopgo.exe'
            OptimizedBinary = Join-Path $FinalDirectory 'manifold-pgo.exe'
            MetadataPath = Join-Path $FinalDirectory 'pgo-metadata.txt'
            EvidencePath = Join-Path $FinalDirectory 'nps-verdict.txt'
        }
        if ($RunComparison) {
            $arguments.RunComparison = $RunComparison
        }
        Invoke-NpsComparison @arguments
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
        Assert-BuildInputsMatchHead
        $networkIdentity = Get-RequiredFileIdentity $networkPath 'embedded network'
        $toolchain = Get-ActiveRustToolchainIdentity
        $profdata = Find-LlvmProfdata $toolchain
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
            Assert-StableFileIdentity $networkPath $networkIdentity.Size $networkIdentity.Hash 'embedded network'

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
            Assert-StableFileIdentity $networkPath $networkIdentity.Size $networkIdentity.Hash 'embedded network'

            Write-Host '== Stage 3/5: merge profiles =='
            $mergedProfile = Join-Path $stagingDirectory 'merged.profdata'
            & $profdata merge -o $mergedProfile ($profraws.FullName)
            $mergeExitCode = $LASTEXITCODE
            Assert-NativeSuccess $mergeExitCode 'llvm-profdata merge' 3
            Assert-StableFileIdentity $networkPath $networkIdentity.Size $networkIdentity.Hash 'embedded network'

            Write-Host '== Stage 4/5: PGO-optimised build =='
            Remove-Item -LiteralPath $optimizedTarget -Recurse -Force -ErrorAction SilentlyContinue
            $optimizedExe = Invoke-CargoBuild "-C target-cpu=native -Cprofile-use=$mergedProfile" $optimizedTarget
            Assert-StableFileIdentity $networkPath $networkIdentity.Size $networkIdentity.Hash 'embedded network'

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
            Assert-BuildInputsMatchHead
            Assert-StableFileIdentity $networkPath $networkIdentity.Size $networkIdentity.Hash 'embedded network'
            if ((Get-ValidatedHeadCommit) -cne $sourceCommit) {
                throw (New-PgoFailure 'ABORT: HEAD changed before PGO metadata generation.' 5)
            }
            $workingTreeBeforePublication = Get-WorkingTreeState
            $metadataPath = Join-Path $stagingDirectory 'pgo-metadata.txt'
            $npsEvidencePath = Join-Path $stagingDirectory 'nps-verdict.txt'
            @(
                "date:         $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
                "commit:       $sourceCommit"
                'build inputs: clean and matched HEAD before build and publication'
                "tree start:   $($workingTreeAtStart.State)"
                "changes start:$($workingTreeAtStart.Details)"
                "tree publish: $($workingTreeBeforePublication.State)"
                "changes pub:  $($workingTreeBeforePublication.Details)"
                "rustc sysroot: $($toolchain.Sysroot)"
                "rustc host:    $($toolchain.Host)"
                'rustc -vV:'
                $toolchain.RustcVerbose
                "llvm-profdata: $profdata"
                'RUSTUP_TOOLCHAIN: cleared; rust-toolchain.toml authoritative'
                'CARGO_ENCODED_RUSTFLAGS: cleared during all Cargo stages'
                'MF_NNUE_TEST_NET: cleared; nets\main.nnue authoritative'
                "baseline:     target\pgo\manifold-nopgo.exe (sha256 $baselineHash)"
                "optimized:    target\pgo\manifold-pgo.exe (sha256 $optimizedHash)"
                "profile:      target\pgo\merged.profdata (sha256 $profileHash)"
                'network:      nets\main.nnue'
                "network size: $($networkIdentity.Size)"
                "network sha256: $($networkIdentity.Hash)"
                "runs:         $BenchRuns x manifold bench"
                "signature:    $nodesAfter nodes (verified unchanged)"
                'nps verdict: pending'
                'nps evidence: pending'
            ) | Set-Content $metadataPath
            if ($MeasureNps) {
                Write-NpsPending $metadataPath $npsEvidencePath
            } else {
                Write-NpsNotMeasured $metadataPath $npsEvidencePath
            }

            Assert-PgoArtifacts $stagingDirectory $sourceCommit $baselineHash $optimizedHash $profileHash `
                $networkPath $networkIdentity.Size $networkIdentity.Hash
            Assert-BuildInputsMatchHead
            Assert-StableFileIdentity $networkPath $networkIdentity.Size $networkIdentity.Hash 'embedded network'
            if ((Get-ValidatedHeadCommit) -cne $sourceCommit) {
                throw (New-PgoFailure 'ABORT: HEAD changed immediately before PGO publication.' 5)
            }
            Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore | Out-Null
            $publishedBaseline = Join-Path $pgoDirectory 'manifold-nopgo.exe'
            $publishedOptimized = Join-Path $pgoDirectory 'manifold-pgo.exe'
            Publish-AndMeasurePgo $stagingDirectory $pgoDirectory $backupDirectory {
                Assert-PgoArtifacts $pgoDirectory $sourceCommit $baselineHash $optimizedHash $profileHash `
                    $networkPath $networkIdentity.Size $networkIdentity.Hash
                Assert-BuildInputsMatchHead
                Assert-StableFileIdentity $networkPath $networkIdentity.Size $networkIdentity.Hash 'embedded network'
                if ((Get-ValidatedHeadCommit) -cne $sourceCommit) {
                    throw (New-PgoFailure 'ABORT: HEAD changed during PGO publication validation.' 5)
                }
                Confirm-OrdinaryReleasePreserved $releaseExistedBefore $releaseHashBefore | Out-Null
            } -MeasureNps:$MeasureNps

            Assert-PgoArtifacts $pgoDirectory $sourceCommit $baselineHash $optimizedHash $profileHash `
                $networkPath $networkIdentity.Size $networkIdentity.Hash
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
