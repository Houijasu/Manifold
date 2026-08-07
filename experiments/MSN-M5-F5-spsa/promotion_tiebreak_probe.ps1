# Is the capture-LMR bestmove disagreement a soundness failure or a tie-break?
#
# `capture_lmr_saves_nodes_on_tactical_middlegames_without_changing_the_move` failed on
# rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8 under the M5-F5 tuned
# defaults: the reduced arm plays d7c8r and the unreduced one d7c8q. Both are promotions
# on the same square. If the two moves score IDENTICALLY the position has two winning
# promotions and the arms merely broke the tie differently, which is not the property the
# test is guarding. This prints the depth-9 score of each arm, and then the score of each
# promotion when it is the ONLY root move allowed (`searchmoves`).
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'
$net    = Join-Path $root 'nets\main.nnue'
$fen    = 'rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8'

function Probe([string]$label, [string[]]$setup, [string]$goCommand) {
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
    foreach ($line in $setup) { & $send $line }
    & $send "position fen $fen"
    & $send $goCommand
    $lastInfo = ''
    $best = ''
    while ($true) {
        $line = $process.StandardOutput.ReadLine()
        if ($null -eq $line) { throw 'engine closed stdout before bestmove' }
        if ($line -match '^info depth') { $lastInfo = $line }
        if ($line -match '^bestmove (\S+)') { $best = $Matches[1]; break }
    }
    & $send 'quit'
    $process.WaitForExit(30000) | Out-Null
    if (-not $process.HasExited) { $process.Kill() }
    $score = if ($lastInfo -match 'score (cp|mate) (-?\d+)') { "$($Matches[1]) $($Matches[2])" } else { '?' }
    $nodes = if ($lastInfo -match '\bnodes (\d+)\b') { $Matches[1] } else { '?' }
    Write-Output ("{0}: bestmove {1}  score {2}  nodes {3}" -f $label, $best, $score, $nodes)
}

$untuned = @(
    'setoption name LmrCoefficient value 2872',
    'setoption name LmrBase value 982',
    'setoption name LmrTtPvReduction value 1024',
    'setoption name LmrHistoryNumerator value 439',
    'setoption name RfpMarginPerDepth value 105',
    'setoption name RfpTtPvMargin value 21',
    'setoption name FutilityBaseMargin value 124',
    'setoption name FutilityMarginPerDepth value 109'
)

Probe 'tuned,   capture LMR ON  (shipped)' @() 'go depth 9'
Probe 'tuned,   capture LMR OFF (control)' @('setoption name UseCaptureLMR value false') 'go depth 9'
Probe 'untuned, capture LMR ON' $untuned 'go depth 9'
Probe 'untuned, capture LMR OFF' ($untuned + 'setoption name UseCaptureLMR value false') 'go depth 9'
