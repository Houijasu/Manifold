# Repeated depth-at-equal-time comparison: defaults vs LmrCoefficient 2000.
#
# A single `go movetime` observation is scheduling jitter, so the claim in the results
# doc is backed by five independent runs of each arm rather than one.
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'
$exe = Join-Path $root 'target\release\manifold.exe'

function Run([int]$coefficient) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute = $false
    $psi.WorkingDirectory = $root
    $p = [System.Diagnostics.Process]::Start($psi)
    $w = $p.StandardInput

    function ReadUntil($proc, [string]$prefix) {
        $lines = @()
        while ($true) {
            $line = $proc.StandardOutput.ReadLine()
            if ($null -eq $line) { break }
            $lines += $line
            if ($line.StartsWith($prefix)) { break }
        }
        $lines
    }

    $w.WriteLine('uci'); ReadUntil $p 'uciok' | Out-Null
    if ($coefficient -ne 2872) { $w.WriteLine("setoption name LmrCoefficient value $coefficient") }
    $w.WriteLine('isready'); ReadUntil $p 'readyok' | Out-Null
    $w.WriteLine('position startpos')
    $w.WriteLine('go movetime 1500')
    $out = ReadUntil $p 'bestmove'
    $w.WriteLine('quit'); $p.WaitForExit(5000) | Out-Null

    $depths = $out | Where-Object { $_ -match '^info depth (\d+) seldepth' } | ForEach-Object {
        if ($_ -match '^info depth (\d+) seldepth') { [int]$Matches[1] }
    }
    ($depths | Measure-Object -Maximum).Maximum
}

$shipped = 1..5 | ForEach-Object { Run 2872 }
$softer = 1..5 | ForEach-Object { Run 2000 }
Write-Output "LmrCoefficient 2872 (shipped): $($shipped -join ', ')  median $(($shipped | Sort-Object)[2])"
Write-Output "LmrCoefficient 2000:           $($softer -join ', ')  median $(($softer | Sort-Object)[2])"
