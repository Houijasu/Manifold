# Collects every bench_cli.rs anchor vector from the release binary in one pass.
#
# Replays each UCI session from crates/mf-uci/tests/bench_cli.rs verbatim and prints
# the "Nodes searched:" vector it produces. Far cheaper than a debug bench_cli run.
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'
$net    = Join-Path $root 'nets\main.nnue'
$tmp    = Join-Path $env:TEMP "mf-anchors-$PID"
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

function Session([string]$label, [string]$script) {
    $inFile  = Join-Path $tmp 'in.txt'
    $outFile = Join-Path $tmp 'out.txt'
    ("setoption name EvalFile value $net`n" + $script) | Out-File -FilePath $inFile -Encoding ascii -NoNewline
    Push-Location $root
    & cmd.exe /c "`"$engine`" < `"$inFile`" > `"$outFile`" 2>&1"
    Pop-Location
    $nodes = Get-Content $outFile | Where-Object { $_ -match '^Nodes searched: (\d+)' } |
             ForEach-Object { [uint64]($_ -replace '^Nodes searched: ', '') }
    Write-Output ("{0}: {1}" -f $label, ($nodes -join ', '))
}

Session 'all-on (BENCH_NODE_COUNT)' "bench`nquit`n"

Session 'ablation (14)' @"
setoption name UseButterflyHistory value false
setoption name UseCaptureHistory value false
setoption name UseContHistory value false
setoption name UseCorrHistory value false
setoption name UseNMP value false
setoption name UseRFP value false
setoption name UseRazoring value false
setoption name UseLMR value false
setoption name UseLMP value false
setoption name UseFutility value false
setoption name UseSEEPruning value false
setoption name UseSingularExt value false
setoption name UseMultiCut value false
setoption name UseIIR value false
setoption name UseProbCut value false
bench
setoption name UseNMP value true
bench
setoption name UseNMP value false
setoption name UseRFP value true
bench
setoption name UseRFP value false
setoption name UseRazoring value true
bench
setoption name UseRazoring value false
setoption name UseLMR value true
bench
setoption name UseLMR value false
setoption name UseLMP value true
bench
setoption name UseLMP value false
setoption name UseFutility value true
bench
setoption name UseFutility value false
setoption name UseSEEPruning value true
bench
setoption name UseSEEPruning value false
setoption name UseSingularExt value true
bench
setoption name UseSingularExt value false
setoption name UseRFP value true
setoption name UseSingularExt value true
bench
setoption name UseIIR value true
bench
setoption name UseIIR value false
setoption name UseRFP value false
setoption name UseSingularExt value false
setoption name UseProbCut value true
bench
setoption name UseProbCut value false
setoption name UseMultiCut value true
bench
setoption name UseMultiCut value false
setoption name UseCheckExt value false
bench
quit
"@

Session 'history toggles (4)' @"
setoption name UseCorrHistory value false
bench
setoption name UseButterflyHistory value false
bench
setoption name UseButterflyHistory value true
setoption name UseCaptureHistory value false
bench
setoption name UseCaptureHistory value true
setoption name UseContHistory value false
bench
quit
"@

Session 'contHist off' "setoption name UseCorrHistory value false`nsetoption name UseContHistory value false`nbench`nquit`n"
Session 'corrHist off'  "setoption name UseCorrHistory value false`nbench`nquit`n"

Session 'correction variants (3)' @"
bench
setoption name UseCorrHistMajor value true
bench
setoption name UseCorrHistMajor value false
setoption name UseCorrHistMaterial value true
bench
quit
"@

Session 'history pruning (2)' "bench`nsetoption name UseHistoryPruning value true`nbench`nquit`n"
Session 'pawn history (2)'    "bench`nsetoption name UsePawnHistory value true`nbench`nquit`n"

Session 'LMR coupling (4)' @"
setoption name UseCorrHistory value false
bench
setoption name UseLMR value false
bench
setoption name UseLMR value true
setoption name UseFutility value false
setoption name UseSEEPruning value false
bench
setoption name UseLMR value false
bench
quit
"@

Session 'qsearch checks (2)' "bench`nsetoption name UseQSearchChecks value true`nbench`nquit`n"
Session 'capture LMR (2)' "bench`nsetoption name UseCaptureLMR value true`nbench`nquit`n"

Remove-Item -Recurse -Force $tmp


