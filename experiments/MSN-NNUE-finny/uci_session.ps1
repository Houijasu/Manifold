# Manual UCI verification for the Finny-table build.
# Blocking StandardOutput.ReadLine() until a sentinel; event handlers fail inside script files.
$ErrorActionPreference = 'Stop'
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = "$PWD\target\release\manifold.exe"
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = "$PWD"
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput
$r = $p.StandardOutput

function Read-Until([string]$prefix, [switch]$Echo) {
    while ($true) {
        $line = $r.ReadLine()
        if ($null -eq $line) { throw "engine closed stdout while waiting for '$prefix'" }
        if ($Echo) { $line }
        if ($line.StartsWith($prefix)) { return $line }
    }
}

$w.WriteLine('uci');     [void](Read-Until 'uciok')
$w.WriteLine('isready'); [void](Read-Until 'readyok')
'--- uci/isready handshake ok ---'

# Endgame with an active king: the position the Finny table most affects.
$w.WriteLine('position fen 8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1')
$w.WriteLine('go movetime 3000')
$last = ''
while ($true) {
    $line = $r.ReadLine()
    if ($line.StartsWith('info depth')) { $last = $line }
    if ($line.StartsWith('bestmove')) { "$last"; "$line"; break }
}

# Chess960 castling, exercising the mirror-flip path through the real protocol.
$w.WriteLine('setoption name UCI_Chess960 value true')
$w.WriteLine('position fen 1rk1r3/8/8/8/8/8/8/1RK1R3 w EBeb - 0 1')
$w.WriteLine('go movetime 2000')
while ($true) {
    $line = $r.ReadLine()
    if ($line.StartsWith('info depth')) { $last = $line }
    if ($line.StartsWith('bestmove')) { "$last"; "$line"; break }
}

"working set (MiB): {0:N1}" -f ((Get-Process -Id $p.Id).WorkingSet64 / 1MB)
$w.WriteLine('quit')
$p.WaitForExit(10000) | Out-Null
"exited: $($p.HasExited)"
