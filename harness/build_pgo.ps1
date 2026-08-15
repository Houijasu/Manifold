<#
.SYNOPSIS
    Reproducible PGO (profile-guided optimisation) build for Manifold.

.DESCRIPTION
    research/rust-perf-and-nnue-training.md section 0.5 verified the full
    -Cprofile-generate / -Cprofile-use round-trip on this MSVC toolchain. This driver
    encodes it so the profile cannot silently rot against the source:

      STAGE 1  Plain release build; the binary is preserved as
               target\release\manifold-nopgo.exe and its `bench` node signature is
               recorded as the reference.
      STAGE 2  Instrumented build (-Cprofile-generate) and `-BenchRuns` `manifold
               bench` invocations. bench is the representative workload: deterministic,
               covers search + eval, and is one command.
      STAGE 3  llvm-profdata merge (found inside the rust-toolchain.toml-pinned
               toolchain; refuses to run with exit 2 if llvm-tools-preview is absent).
      STAGE 4  Optimised build with -Cprofile-use.
      STAGE 5  Verification gate: the PGO binary's `bench` node signature MUST equal
               the stage-1 reference. A drift means the optimiser changed the search,
               which is a defect, not a speedup -- abort with exit 4.

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

$nopgo = 'target\release\manifold-nopgo.exe'
$exe   = 'target\release\manifold.exe'

function Invoke-CargoBuild {
    param([string]$RustFlags)
    if ($RustFlags) { $env:RUSTFLAGS = $RustFlags } else { Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue }
    cargo build --release -p mf-uci
    if ($LASTEXITCODE -ne 0) { Write-Host "ABORT: cargo build failed (exit $LASTEXITCODE)"; exit 3 }
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
Invoke-CargoBuild ''
Copy-Item $exe $nopgo -Force
$nodesBefore = Get-BenchSignature $exe
Write-Host "reference signature: $nodesBefore nodes"

Write-Host '== Stage 2/5: instrumented build and profiling runs =='
if (Test-Path $PgoDir) { Remove-Item $PgoDir -Recurse -Force }
New-Item -ItemType Directory -Path $PgoDir | Out-Null
Invoke-CargoBuild "-C target-cpu=native -Cprofile-generate=$((Resolve-Path $PgoDir).Path)"
1..$BenchRuns | ForEach-Object {
    & ".\$exe" bench | Out-Null
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
Invoke-CargoBuild "-C target-cpu=native -Cprofile-use=$((Resolve-Path $PgoDir).Path)\merged.profdata"
Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue

Write-Host '== Stage 5/5: node-signature verification =='
$nodesAfter = Get-BenchSignature $exe
if ($nodesAfter -ne $nodesBefore) {
    Write-Host "ABORT: node signature drifted ($nodesBefore -> $nodesAfter). The optimiser changed the search."
    exit 4
}
Write-Host "signature verified: $nodesAfter nodes (unchanged)"

# Provenance, in the run_match.ps1 spirit: a build without metadata is not evidence.
@(
    "date:      $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
    "commit:    $(git rev-parse --short HEAD)"
    "rustc:     $(rustc --version)"
    "profile:   $PgoDir\merged.profdata (sha256 $((Get-FileHash "$PgoDir\merged.profdata").Hash))"
    "runs:      $BenchRuns x manifold bench"
    "signature: $nodesAfter nodes (verified unchanged)"
) | Set-Content "$PgoDir\pgo-metadata.txt"

if ($MeasureNps) {
    Write-Host '== NPS comparison (baseline vs PGO) =='
    py -3.14 harness\nps_compare.py --engine "A=$nopgo" --engine "B=$exe" --depth 12 --hash 64 --warmup 1 --repeat 3
}

Write-Host "PGO build complete: $exe (baseline preserved at $nopgo)"
