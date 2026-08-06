# Live UCI verification that UseTimeEffort reaches the CLOCK and nothing else.
#
# Driver notes (library/user-testing.md): inside a script file, draining engine stdout
# via add_OutputDataReceived or [Task]::Run fails with "no Runspace available", and
# redirecting stdin from a FILE delivers `quit` before a timed `go` has finished, which
# aborts the search. So: a real Process with a live stdin writer, and blocking
# StandardOutput.ReadLine() until a sentinel line.
#
# Prints, per position and per toggle state: wall time of the `go`, the final depth
# reached, and the bestmove. The clocked arm is where the term acts; the `go depth`
# arm is the control that must be bit-identical in node count.
param(
    [string]$Engine = 'C:\Users\Samaritan\Projects\Manifold\target\release\manifold.exe'
)
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'

function Start-Engine {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName               = $Engine
    $psi.WorkingDirectory       = $root
    $psi.RedirectStandardInput  = $true
    $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute        = $false
    [System.Diagnostics.Process]::Start($psi)
}

function Read-Until([System.Diagnostics.Process]$p, [string]$sentinel) {
    $lines = @()
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        $lines += $line
        if ($line -match $sentinel) { break }
    }
    $lines
}

function Probe([string]$fen, [bool]$effort, [string]$go) {
    $p = Start-Engine
    $p.StandardInput.WriteLine('uci')
    Read-Until $p 'uciok' | Out-Null
    $p.StandardInput.WriteLine("setoption name UseTimeEffort value $($effort.ToString().ToLower())")
    $p.StandardInput.WriteLine('setoption name Hash value 64')
    $p.StandardInput.WriteLine('ucinewgame')
    $p.StandardInput.WriteLine("position fen $fen")
    $p.StandardInput.WriteLine('isready')
    Read-Until $p 'readyok' | Out-Null
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $p.StandardInput.WriteLine($go)
    $out = Read-Until $p '^bestmove'
    $sw.Stop()
    $p.StandardInput.WriteLine('quit')
    $p.WaitForExit(10000) | Out-Null

    $info = $out | Where-Object { $_ -match '^info depth .* multipv 1' } | Select-Object -Last 1
    $depth = if ($info -match 'info depth (\d+)') { [int]$Matches[1] } else { 0 }
    $nodes = if ($info -match ' nodes (\d+)') { [uint64]$Matches[1] } else { 0 }
    $best  = ($out | Where-Object { $_ -match '^bestmove' } | Select-Object -First 1)
    [pscustomobject]@{
        Effort  = $effort
        Go      = $go
        WallMs  = [int]$sw.Elapsed.TotalMilliseconds
        Depth   = $depth
        Nodes   = $nodes
        Best    = ($best -replace '^bestmove ', '' -replace ' ponder.*', '')
    }
}

# Positions: startpos, kiwipete, a quiet endgame, and a sharp tactical middlegame.
$positions = @(
    @{ Name = 'startpos';    Fen = 'rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1' },
    @{ Name = 'kiwipete';    Fen = 'r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1' },
    @{ Name = 'endgame';     Fen = '8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1' },
    @{ Name = 'tactical';    Fen = '2kr3r/pp1q1ppp/5n2/1Nb5/2Pp1B2/7Q/P4PPP/1R3RK1 w - - 0 1' }
)

$gos = @(
    @{ Label = 'clock 8+0.08'; Go = 'go wtime 8000 btime 8000 winc 80 binc 80' },
    @{ Label = 'depth 12';     Go = 'go depth 12' }
)

Write-Output 'name         go            effort  wall_ms  depth      nodes  bestmove'
Write-Output '------------ ------------- ------- -------- ------ ---------- --------'
foreach ($pos in $positions) {
    foreach ($go in $gos) {
        foreach ($effort in @($true, $false)) {
            $r = Probe $pos.Fen $effort $go.Go
            Write-Output ('{0,-12} {1,-13} {2,-7} {3,8} {4,6} {5,10}  {6}' -f `
                $pos.Name, $go.Label, $r.Effort, $r.WallMs, $r.Depth, $r.Nodes, $r.Best)
        }
    }
}
