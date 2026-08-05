# Manual UCI verification for the large-Hash fix (M1-F2).
#
# Uses the Process-based driver: piping a here-string closes stdin, which aborts
# `go movetime`. Output is drained with blocking ReadLine() calls on the main thread --
# PowerShell event handlers and Tasks fail with "no Runspace available" inside a script.
param([int]$HashMib = 8096, [int]$MoveTimeMs = 10000)

$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = "$root\target\release\manifold.exe"
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $root
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput

$collected = New-Object System.Collections.Generic.List[string]
function Read-Until([string]$sentinel) {
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        $collected.Add($line)
        if ($line -eq $sentinel -or $line -like "$sentinel*") { break }
    }
}

$w.WriteLine('uci')
Read-Until 'uciok'

$w.WriteLine("setoption name Hash value $HashMib")
$w.WriteLine('isready')
$allocStart = Get-Date
Read-Until 'readyok'
$allocSeconds = ((Get-Date) - $allocStart).TotalSeconds
$proc = Get-Process -Id $p.Id
Write-Host ("Allocation wall time : {0:N2} s" -f $allocSeconds)
Write-Host ("WorkingSet64 at Hash={0}: {1:N0} bytes ({2:N2} GB)" -f $HashMib, $proc.WorkingSet64, ($proc.WorkingSet64 / 1GB))

$w.WriteLine('ucinewgame')
$w.WriteLine('isready')
Read-Until 'readyok'

$w.WriteLine('position startpos')
$w.WriteLine("go movetime $MoveTimeMs")
Read-Until 'bestmove'
$proc.Refresh()
Write-Host ("WorkingSet64 after search: {0:N2} GB" -f ($proc.WorkingSet64 / 1GB))

$w.WriteLine('quit')
$p.WaitForExit(20000) | Out-Null

Write-Host "`n--- handshake Hash line ---"
$collected | Where-Object { $_ -like 'option name Hash*' }
Write-Host "`n--- info string lines ---"
$collected | Where-Object { $_ -like 'info string*' }
Write-Host "`n--- hashfull progression ---"
$collected | Where-Object { $_ -match 'hashfull' } | ForEach-Object {
    if ($_ -match 'depth (\d+).*nodes (\d+).*hashfull (\d+)') {
        "depth {0,2}  nodes {1,12}  hashfull {2}" -f $Matches[1], $Matches[2], $Matches[3]
    }
}
Write-Host "`n--- bestmove ---"
$collected | Where-Object { $_ -like 'bestmove*' }
