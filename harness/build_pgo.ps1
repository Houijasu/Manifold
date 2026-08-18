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
               which is a defect, not a speedup -- abort with exit 4. Verified baseline
               and PGO copies are published under target\pgo without touching the
               ordinary target\release\manifold.exe.

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
    [int]$BenchRuns = 3,

    # Profile workspace (profraw + merged.profdata + provenance). Deleted and
    # recreated on every run so a stale profile can never survive a source change.
    [string]$PgoDir = 'target\pgo',

    # After stage 5, run harness\nps_compare.py (nopgo vs pgo) for a measured verdict.
    [switch]$MeasureNps
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)   # repo root

$baselineTarget = 'target\pgo-build\baseline'
$instrumentedTarget = 'target\pgo-build\instrumented'
$optimizedTarget = 'target\pgo-build\optimized'
$nopgo = Join-Path $PgoDir 'manifold-nopgo.exe'
$pgo = Join-Path $PgoDir 'manifold-pgo.exe'

function Invoke-CargoBuild {
    param(
        [string]$RustFlags,
        [string]$TargetDir
    )
    if ($RustFlags) { $env:RUSTFLAGS = $RustFlags } else { Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue }
    $env:CARGO_TARGET_DIR = $TargetDir
    cargo build --release -p mf-uci
    if ($LASTEXITCODE -ne 0) { Write-Host "ABORT: cargo build failed (exit $LASTEXITCODE)"; exit 3 }
    return Join-Path $TargetDir 'release\manifold.exe'
}

function Get-BenchSignature {
    param([string]$Binary)
    $out = & ".\$Binary" bench 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { Write-Host "ABORT: bench failed on $Binary (exit $LASTEXITCODE)"; exit 5 }
    if ($out -notmatch 'Nodes searched:\s*(\d+)') { Write-Host "ABORT: no node signature in bench output of $Binary"; exit 5 }
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
    exit 2
}

Write-Host '== Stage 1/5: baseline build and reference signature =='
if (Test-Path $PgoDir) { Remove-Item $PgoDir -Recurse -Force }
New-Item -ItemType Directory -Path $PgoDir | Out-Null
$profileDirectory = (Resolve-Path $PgoDir).Path
$baselineExe = Invoke-CargoBuild '' $baselineTarget
$nodesBefore = Get-BenchSignature $baselineExe
Write-Host "reference signature: $nodesBefore nodes"

Write-Host '== Stage 2/5: instrumented build and profiling runs =='
$instrumentedExe = Invoke-CargoBuild "-C target-cpu=native -Cprofile-generate=$profileDirectory" $instrumentedTarget
1..$BenchRuns | ForEach-Object {
    & ".\$instrumentedExe" bench | Out-Null
    if ($LASTEXITCODE -ne 0) { Write-Host "ABORT: profiling run $_ failed (exit $LASTEXITCODE)"; exit 5 }
}
$profraws = Get-ChildItem $PgoDir -Filter '*.profraw'
if ($profraws.Count -eq 0) { Write-Host "ABORT: no .profraw produced in $PgoDir"; exit 5 }
Write-Host "collected $($profraws.Count) profraw file(s)"

Write-Host '== Stage 3/5: merge profiles =='
$profdata = Find-LlvmProfdata
& $profdata merge -o "$PgoDir\merged.profdata" ($profraws.FullName)
if ($LASTEXITCODE -ne 0) { Write-Host "ABORT: llvm-profdata merge failed (exit $LASTEXITCODE)"; exit 3 }

Write-Host '== Stage 4/5: PGO-optimised build =='
if (Test-Path $optimizedTarget) { Remove-Item $optimizedTarget -Recurse -Force }
$optimizedExe = Invoke-CargoBuild "-C target-cpu=native -Cprofile-use=$profileDirectory\merged.profdata" $optimizedTarget
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue

Write-Host '== Stage 5/5: node-signature verification =='
$nodesAfter = Get-BenchSignature $optimizedExe
if ($nodesAfter -ne $nodesBefore) {
    Write-Host "ABORT: node signature drifted ($nodesBefore -> $nodesAfter). The optimiser changed the search."
    exit 4
}
Write-Host "signature verified: $nodesAfter nodes (unchanged)"

# Provenance, in the run_match.ps1 spirit: a build without metadata is not evidence.
$sourceCommit = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $sourceCommit -notmatch '\A[0-9a-fA-F]{40}\z') {
    Write-Host 'ABORT: git did not return a valid 40-hex source commit.'
    exit 5
}
Copy-Item $baselineExe $nopgo -Force
Copy-Item $optimizedExe $pgo -Force
[System.IO.File]::WriteAllText((Join-Path (Get-Location) "$nopgo.source-commit"), $sourceCommit)
[System.IO.File]::WriteAllText((Join-Path (Get-Location) "$pgo.source-commit"), $sourceCommit)
$baselineHash = (Get-FileHash $nopgo -Algorithm SHA256).Hash
$optimizedHash = (Get-FileHash $pgo -Algorithm SHA256).Hash
$profileHash = (Get-FileHash "$PgoDir\merged.profdata" -Algorithm SHA256).Hash
@(
    "date:      $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
    "commit:    $sourceCommit"
    "rustc:     $(rustc --version)"
    "baseline:  $nopgo (sha256 $baselineHash)"
    "optimized: $pgo (sha256 $optimizedHash)"
    "profile:   $PgoDir\merged.profdata (sha256 $profileHash)"
    "runs:      $BenchRuns x manifold bench"
    "signature: $nodesAfter nodes (verified unchanged)"
) | Set-Content "$PgoDir\pgo-metadata.txt"

if ($MeasureNps) {
    Write-Host '== NPS comparison (baseline vs PGO) =='
    py -3.14 harness\nps_compare.py --engine "A=$nopgo" --engine "B=$pgo" --depth 12 --hash 64 --warmup 1 --repeat 3
}

Write-Host "Experimental PGO outputs complete: $pgo (baseline at $nopgo)."
Write-Host 'These are experiment artifacts, not shipping/release artifacts; target\release\manifold.exe was not modified.'
