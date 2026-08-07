# Per-position check of "a 20% smaller LmrCoefficient searches MORE nodes".
#
# `changing_the_lmr_coefficient_changes_fixed_depth_node_counts` asserts it on Kiwipete
# alone at depth 8, and that assertion inverted under the M5-F5 tuned default. The bench
# suite is monotone across the whole advertised range (lmr_monotonicity_probe.ps1), so
# this asks how many INDIVIDUAL positions carry the direction, at the depth the test uses.
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'
$net    = Join-Path $root 'nets\main.nnue'

$positions = @(
    'r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1',
    'r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10',
    '2kr3r/pp1q1ppp/5n2/1Nb5/2Pp1B2/7Q/P4PPP/1R3RK1 w - - 0 1',
    'rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8',
    'r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1'
)

function NodesFor([int]$coefficient, [int]$depth) {
    $info = New-Object System.Diagnostics.ProcessStartInfo
    $info.FileName = $engine
    $info.WorkingDirectory = $root
    $info.RedirectStandardInput = $true
    $info.RedirectStandardOutput = $true
    $info.UseShellExecute = $false
    $process = [System.Diagnostics.Process]::Start($info)
    $send = {
        param($line)
        $process.StandardInput.WriteLine($line)
        $process.StandardInput.Flush()
    }
    & $send "setoption name EvalFile value $net"
    & $send "setoption name LmrCoefficient value $coefficient"
    $totals = @()
    foreach ($fen in $positions) {
        & $send 'ucinewgame'
        & $send "position fen $fen"
        & $send "go depth $depth"
        $last = [uint64]0
        while ($true) {
            $line = $process.StandardOutput.ReadLine()
            if ($null -eq $line) { throw 'engine closed stdout before bestmove' }
            if ($line -match '^info .*\bnodes (\d+)\b') { $last = [uint64]$Matches[1] }
            if ($line -match '^bestmove') { break }
        }
        $totals += $last
    }
    & $send 'quit'
    $process.WaitForExit(30000) | Out-Null
    if (-not $process.HasExited) { $process.Kill() }
    return $totals
}

foreach ($depth in 8, 9) {
    $shipped = NodesFor 2754 $depth
    $softer  = NodesFor 2203 $depth
    Write-Output "--- depth $depth (shipped 2754 vs 20%-smaller 2203) ---"
    for ($i = 0; $i -lt $positions.Count; $i++) {
        $verdict = if ($softer[$i] -gt $shipped[$i]) { 'softer searches MORE (expected)' } else { 'INVERTED' }
        Write-Output ("position {0}: shipped {1}  softer {2}  {3}" -f ($i + 1), $shipped[$i], $softer[$i], $verdict)
    }
    $sumShipped = [uint64](($shipped | Measure-Object -Sum).Sum)
    $sumSofter  = [uint64](($softer  | Measure-Object -Sum).Sum)
    Write-Output ("TOTAL: shipped {0}  softer {1}  ratio {2}" -f $sumShipped, $sumSofter, [math]::Round([double]$sumSofter / [double]$sumShipped, 3))
}
