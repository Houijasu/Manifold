# Does capture history still SAVE nodes under the M5-F5 tuned parameters?
#
# `bench_cli`'s history-toggle anchor asserts it does. Under the tuned LMR/RFP/futility
# values that depth-7 reading FLIPPED sign by 0.85% (39,134 with capture history on vs
# 38,803 with it off). This probe asks whether the flip is a depth-7 artefact by repeating
# the same on/off comparison at depths 7, 10, 12 and 14 over six tactical positions, with
# correction history off exactly as the anchor session has it.
#
# Driver note (mission AGENTS.md rule 7): a piped here-string closes stdin, and `quit`
# then aborts the running `go`, which silently yields a depth-2 answer to a depth-12
# question. Commands are therefore written one at a time and stdout is drained with
# blocking ReadLine() until each search's own `bestmove` sentinel.
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'
$net    = Join-Path $root 'nets\main.nnue'

$positions = @(
    'r1bqkbnr/pppp1ppp/2n5/4p3/2B1P3/5Q2/PPPP1PPP/RNB1K1NR w KQkq - 4 4',
    'r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1',
    '8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1',
    'r2q1rk1/pP1p2pp/Q4n2/bbp1p3/Np6/1B3NBn/pPPP1PPP/R3K2R b KQ - 0 1',
    '2kr3r/p1ppqpb1/bn2Qnp1/3PN3/1p2P3/2N5/PPPBBPPP/R3K2R b KQ - 3 2',
    'rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8'
)

function NodesFor([bool]$captureHistory, [int]$depth) {
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
    & $send 'setoption name UseCorrHistory value false'
    & $send "setoption name UseCaptureHistory value $($captureHistory.ToString().ToLower())"

    $totals = @()
    foreach ($fen in $positions) {
        & $send 'ucinewgame'
        & $send "position fen $fen"
        & $send "go depth $depth"
        $last = [uint64]0
        while ($true) {
            $line = $process.StandardOutput.ReadLine()
            if ($null -eq $line) { throw "engine closed stdout before bestmove" }
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

Write-Output "depth,captureHistoryOn,captureHistoryOff,deltaPercentFromEnabling"
foreach ($depth in 7, 10, 12, 14) {
    $on  = NodesFor $true  $depth
    $off = NodesFor $false $depth
    $sumOn  = [uint64](($on  | Measure-Object -Sum).Sum)
    $sumOff = [uint64](($off | Measure-Object -Sum).Sum)
    $delta = 100.0 * ([double]$sumOn - [double]$sumOff) / [double]$sumOff
    Write-Output ("{0},{1},{2},{3}" -f $depth, $sumOn, $sumOff, [math]::Round($delta, 2))
}
