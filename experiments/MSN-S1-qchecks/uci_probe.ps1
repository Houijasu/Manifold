# Live UCI verification of UseQSearchChecks.
#
# Driver notes (library/user-testing.md): inside a script file, draining engine stdout
# via add_OutputDataReceived or [Task]::Run fails with "no Runspace available", and
# redirecting stdin from a FILE delivers `quit` before `go movetime` has finished, which
# aborts the search. So: a real Process with a live stdin writer, and blocking
# StandardOutput.ReadLine() until a sentinel line.
$ErrorActionPreference = 'Stop'
$root   = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'

function Start-Engine {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName               = $engine
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

function Probe([string]$label, [string[]]$setup, [string]$go) {
    $p = Start-Engine
    $p.StandardInput.WriteLine('uci')
    $handshake = Read-Until $p 'uciok'
    foreach ($line in $setup) { $p.StandardInput.WriteLine($line) }
    $p.StandardInput.WriteLine('isready')
    Read-Until $p 'readyok' | Out-Null
    $p.StandardInput.WriteLine($go)
    $search = Read-Until $p '^bestmove'
    $p.StandardInput.WriteLine('quit')
    $p.WaitForExit(10000) | Out-Null
    Write-Output "===== $label ====="
    # Only the summary line of each iteration and the bestmove; currmove spam dropped.
    ($handshake + $search) | Where-Object {
        $_ -match 'UseQSearchChecks' -or $_ -eq 'uciok' -or $_ -match 'multipv 1' -or $_ -match '^bestmove'
    } | ForEach-Object { Write-Output $_ }
}

$mate       = 'position fen 3r3k/8/8/8/8/6q1/P7/7K w - - 0 1'
$middlegame = 'position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1'
$off        = 'setoption name UseQSearchChecks value false'

Probe 'quiet mate at depth 1, checks ON'  @($mate)       'go depth 1'
Probe 'quiet mate at depth 1, checks OFF' @($off, $mate) 'go depth 1'
Probe 'middlegame movetime 3000, checks ON'  @($middlegame)       'go movetime 3000'
Probe 'middlegame movetime 3000, checks OFF' @($off, $middlegame) 'go movetime 3000'
