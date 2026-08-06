# Reproduces uci_protocol::fixed_depth_output_is_identical_at_every_thread_count
# against an arbitrary binary, to separate a defect this feature introduced from a
# pre-existing coupling it merely exposed.
param([Parameter(Mandatory = $true)][string]$Engine, [string[]]$Setup = @())
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'

function Run([int]$threads) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Engine
    $psi.WorkingDirectory = $root
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute = $false
    $p = [System.Diagnostics.Process]::Start($psi)
    $p.StandardInput.WriteLine('uci')
    while ($true) { if ($p.StandardOutput.ReadLine() -eq 'uciok') { break } }
    foreach ($line in $Setup) { $p.StandardInput.WriteLine($line) }
    $p.StandardInput.WriteLine("setoption name Threads value $threads")
    $p.StandardInput.WriteLine('position startpos moves e2e4 e7e5 g1f3')
    $p.StandardInput.WriteLine('go depth 8')
    $lines = @()
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        if ($line -match 'multipv 1') { $lines += ($line -replace ' (nps|time) \d+', '') }
        if ($line -match '^bestmove') { $lines += $line; break }
    }
    $p.StandardInput.WriteLine('quit')
    $p.WaitForExit(10000) | Out-Null
    $lines
}

$one = Run 1
foreach ($threads in @(2, 8)) {
    $many = Run $threads
    $same = ($one -join "`n") -eq ($many -join "`n")
    Write-Output "Threads=1 vs Threads=${threads}: $(if ($same) { 'IDENTICAL' } else { 'DIFFERENT' })"
    if (-not $same) {
        for ($i = 0; $i -lt [Math]::Max($one.Count, $many.Count); $i++) {
            if ($one[$i] -ne $many[$i]) {
                Write-Output "  1T: $($one[$i])"
                Write-Output "  ${threads}T: $($many[$i])"
            }
        }
    }
}
