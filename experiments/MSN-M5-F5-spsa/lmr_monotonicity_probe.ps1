# Is "a smaller LmrCoefficient searches more nodes" still true under the tuned defaults?
#
# Two tests assert it (bench_cli's tunable-wiring anchor and search_invariants'
# `changing_the_lmr_coefficient_changes_fixed_depth_node_counts`), and the second one
# FAILED after M5-F5 re-based the default from 2872 to 2754 on ONE position at depth 8.
# This sweeps the coefficient across its whole advertised range on the bench suite, to
# separate "the monotonicity is gone" from "that one position is a local inversion".
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'
$net    = Join-Path $root 'nets\main.nnue'
$tmp    = Join-Path $env:TEMP "mf-lmrsweep-$PID"
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

$lines = @("setoption name EvalFile value $net")
$values = @(1000, 1500, 2000, 2203, 2400, 2600, 2754, 2900, 3200, 3600, 4200, 5000, 6000)
foreach ($v in $values) {
    $lines += "setoption name LmrCoefficient value $v"
    $lines += 'bench'
}
$lines += 'quit'

$inFile  = Join-Path $tmp 'in.txt'
$outFile = Join-Path $tmp 'out.txt'
($lines -join "`n") + "`n" | Out-File -FilePath $inFile -Encoding ascii -NoNewline
Push-Location $root
& cmd.exe /c "`"$engine`" < `"$inFile`" > `"$outFile`" 2>&1"
Pop-Location

$nodes = Get-Content $outFile | Where-Object { $_ -match '^Nodes searched: (\d+)' } |
         ForEach-Object { [uint64]($_ -replace '^Nodes searched: ', '') }

Write-Output "LmrCoefficient,benchNodes"
for ($i = 0; $i -lt $values.Count; $i++) {
    Write-Output ("{0},{1}" -f $values[$i], $nodes[$i])
}

Remove-Item -Recurse -Force $tmp
