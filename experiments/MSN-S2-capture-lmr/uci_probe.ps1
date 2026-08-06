# Live UCI verification of UseCaptureLMR through a real session.
#
# Uses a System.Diagnostics.Process with a live stdin writer and blocking
# StandardOutput.ReadLine(); piping/redirecting stdin from a file closes stdin and aborts
# `go movetime` (library/user-testing.md).
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'
$engine = Join-Path $root 'target\release\manifold.exe'

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

function Probe([string]$label, [string[]]$setup, [string]$position, [string]$go) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $engine; $psi.WorkingDirectory = $root
    $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute = $false
    $p = [System.Diagnostics.Process]::Start($psi)
    $w = $p.StandardInput
    $w.WriteLine('uci')
    $handshake = Read-Until $p 'uciok'
    foreach ($line in $setup) { $w.WriteLine($line) }
    $w.WriteLine('setoption name Hash value 64')
    $w.WriteLine('ucinewgame'); $w.WriteLine('isready')
    $ready = Read-Until $p 'readyok'
    $w.WriteLine("position $position")
    $w.WriteLine($go)
    $search = Read-Until $p '^bestmove'
    $mem = (Get-Process -Id $p.Id -ErrorAction SilentlyContinue).WorkingSet64
    $w.WriteLine('quit'); $p.WaitForExit(10000) | Out-Null

    Write-Output "===== $label ====="
    ($handshake | Where-Object { $_ -match 'UseCaptureLMR' }) | ForEach-Object { Write-Output "handshake: $_" }
    ($ready | Where-Object { $_ -match 'evaluation NNUE|hash resized' }) | ForEach-Object { Write-Output $_ }
    ($search | Where-Object { $_ -match 'multipv 1' } | Select-Object -Last 3) | ForEach-Object { Write-Output $_ }
    ($search | Where-Object { $_ -match '^bestmove' }) | ForEach-Object { Write-Output $_ }
    Write-Output ("WorkingSet64: {0:N0} bytes" -f $mem)
    Write-Output ''
}

$mid = 'fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1'
$on  = 'setoption name UseCaptureLMR value true'
$off = 'setoption name UseCaptureLMR value false'

Probe 'kiwipete movetime 3000, capture LMR ON (default)'  @()     $mid 'go movetime 3000'
Probe 'kiwipete movetime 3000, capture LMR OFF'           @($off) $mid 'go movetime 3000'
Probe 'kiwipete depth 14, capture LMR ON'                 @($on)  $mid 'go depth 14'
Probe 'kiwipete depth 14, capture LMR OFF'                @($off) $mid 'go depth 14'
