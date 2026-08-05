# M2-F1 manual UCI verification: confirms the profiling feature changed nothing observable
# through the real protocol surface. Blocking ReadLine (no event handlers, no Tasks) per
# library/user-testing.md.
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = Join-Path $root "target\release\manifold.exe"
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $root
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput

function Read-Until([string]$sentinel, [int]$timeoutSeconds) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    $lines = @()
    while ((Get-Date) -lt $deadline) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        $lines += $line
        if ($line.StartsWith($sentinel)) { return $lines }
    }
    throw "timed out waiting for '$sentinel'"
}

$w.WriteLine('uci')
$handshake = Read-Until 'uciok' 60
Write-Host "--- handshake (Hash option + uciok) ---"
$handshake | Where-Object { $_ -match 'name Hash |^uciok|^id name' }

$w.WriteLine('setoption name Hash value 64')
$w.WriteLine('isready')
$ready = Read-Until 'readyok' 60
Write-Host "--- isready ---"
$ready

$w.WriteLine('ucinewgame')
$w.WriteLine('isready')
Read-Until 'readyok' 60 | Out-Null

$w.WriteLine('position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1')
$w.WriteLine('go movetime 3000')
$search = Read-Until 'bestmove' 60
Write-Host "--- last 3 info lines + bestmove (kiwipete, movetime 3000) ---"
$search | Where-Object { $_ -match '^info depth' } | Select-Object -Last 3
$search | Where-Object { $_ -match '^bestmove' }

Write-Host "--- working set while alive ---"
Get-Process -Id $p.Id | Select-Object -ExpandProperty WorkingSet64

$w.WriteLine('quit')
$p.WaitForExit(10000) | Out-Null
Write-Host "exit code: $($p.ExitCode)"
