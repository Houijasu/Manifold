# Manual UCI verification of the M5-F3 tunable spin options.
#
# Uses the blocking-ReadLine driver mandated by library/user-testing.md: piped
# here-strings close stdin and abort `go movetime`, and `add_OutputDataReceived`
# has no Runspace inside a script file.
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = Join-Path $root 'target\release\manifold.exe'
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $root
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput

function ReadUntil([string]$sentinel) {
    $lines = @()
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        $lines += $line
        if ($line -eq $sentinel -or $line.StartsWith($sentinel)) { break }
    }
    $lines
}

$w.WriteLine('uci')
$handshake = ReadUntil 'uciok'
$spins = $handshake | Where-Object { $_ -match '^option name \w+ type spin' }
Write-Output "=== handshake spins ($($spins.Count)) ==="
$spins | ForEach-Object { Write-Output $_ }

$w.WriteLine('isready'); ReadUntil 'readyok' | Out-Null

Write-Output "`n=== go movetime 1500 at DEFAULTS ==="
$w.WriteLine('position startpos')
$w.WriteLine('go movetime 1500')
$default = ReadUntil 'bestmove'
$default | Select-Object -Last 3 | ForEach-Object { Write-Output $_ }

Write-Output "`n=== go movetime 1500 with LmrCoefficient 2000 ==="
$w.WriteLine('setoption name LmrCoefficient value 2000')
$w.WriteLine('ucinewgame')
$w.WriteLine('isready'); ReadUntil 'readyok' | Out-Null
$w.WriteLine('position startpos')
$w.WriteLine('go movetime 1500')
$tuned = ReadUntil 'bestmove'
$tuned | Select-Object -Last 3 | ForEach-Object { Write-Output $_ }

Write-Output "`n=== an out-of-range and an unparseable write ==="
$w.WriteLine('setoption name RfpMarginPerDepth value 99999')
$w.WriteLine('setoption name RfpMarginPerDepth value banana')
$w.WriteLine('isready')
ReadUntil 'readyok' | ForEach-Object { Write-Output $_ }

$w.WriteLine('quit')
$p.WaitForExit(5000) | Out-Null
Write-Output "`nexit code: $($p.ExitCode)"
