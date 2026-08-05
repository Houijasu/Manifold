# Manual UCI verification for M2-F2 (lazy accumulator updates).
#
# Drives the release engine through a real UCI session, exercising the paths lazy updates
# touch hardest: deep searches (long pending chains through pruned subtrees), an endgame with
# an active king (both Finny mirror tiers reached through deferred frames), and Chess960
# king-takes-rook castling. Uses blocking ReadLine rather than add_OutputDataReceived, which
# fails with "no Runspace available" inside a script file.

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = Join-Path $root 'target\release\manifold.exe'
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $root
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput

function Read-Until([string]$sentinel) {
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        Write-Output $line
        if ($line.StartsWith($sentinel)) { break }
    }
}

$w.WriteLine('uci');     Read-Until 'uciok'
$w.WriteLine('isready'); Read-Until 'readyok'

Write-Output '=== startpos, go depth 18 (long deferred chains under pruning) ==='
$w.WriteLine('position startpos')
$w.WriteLine('go depth 18')
Read-Until 'bestmove'

Write-Output '=== endgame with an active king, go movetime 3000 ==='
$w.WriteLine('position fen 8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1')
$w.WriteLine('go movetime 3000')
Read-Until 'bestmove'

Write-Output '=== kiwipete, go movetime 2000 ==='
$w.WriteLine('position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1')
$w.WriteLine('go movetime 2000')
Read-Until 'bestmove'

Write-Output '=== Chess960 king-takes-rook castling, go movetime 2000 ==='
$w.WriteLine('setoption name UCI_Chess960 value true')
$w.WriteLine('position fen 1rk1r3/8/8/8/8/8/8/1RK1R3 w EBeb - 0 1')
$w.WriteLine('go movetime 2000')
Read-Until 'bestmove'

$memory = (Get-Process -Id $p.Id).WorkingSet64
Write-Output "WorkingSet64 = $([math]::Round($memory / 1MB, 1)) MiB"

$w.WriteLine('quit')
$p.WaitForExit(10000) | Out-Null
Write-Output "exit code = $($p.ExitCode)"
