# The bench_cli.rs anchor sessions collect_anchors.ps1 does not cover.
#
# Kept alongside it rather than merged because collect_anchors.ps1 is copied verbatim
# from MSN-S1 and its diff against the original is the record of what M4-F1b changed.
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'
$net    = Join-Path $root 'nets\main.nnue'
$tmp    = Join-Path $env:TEMP "mf-anchors2-$PID"
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

function Session([string]$label, [string]$script) {
    $inFile  = Join-Path $tmp 'in.txt'
    $outFile = Join-Path $tmp 'out.txt'
    ("setoption name EvalFile value $net`n" + $script) | Out-File -FilePath $inFile -Encoding ascii -NoNewline
    Push-Location $root
    & cmd.exe /c "`"$engine`" < `"$inFile`" > `"$outFile`" 2>&1"
    Pop-Location
    $nodes = Get-Content $outFile | Where-Object { $_ -match '^Nodes searched: (\d+)' } |
             ForEach-Object { $_ -replace '^Nodes searched: ', '' }
    Write-Output ("{0}: {1}" -f $label, ($nodes -join ', '))
}

Session 'postlmr conthist (2)'  "bench`nsetoption name UsePostLMRContHist value true`nbench`nquit`n"
Session 'postlmr depth off (2)' "bench`nsetoption name UsePostLMRDepth value false`nbench`nquit`n"
Session 'postlmr without lmr (3)' @"
setoption name UseLMR value false
bench
setoption name UsePostLMRDepth value false
bench
setoption name UsePostLMRContHist value true
bench
quit
"@
Session 'threads4 bench (4)' @"
setoption name Threads value 4
bench
bench
bench
setoption name Hash value 64
ucinewgame
bench
quit
"@
Session 'time effort (3)' @"
bench
setoption name UseTimeEffort value true
bench
setoption name UseTimeEffort value false
bench
quit
"@
Session 'M3 signature (captureLMR off + postLMRdepth off)' @"
setoption name UseCaptureLMR value false
setoption name UsePostLMRDepth value false
bench
quit
"@

Remove-Item -Recurse -Force $tmp
