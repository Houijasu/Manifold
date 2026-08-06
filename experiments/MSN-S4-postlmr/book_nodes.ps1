<#
.SYNOPSIS
    Fixed-depth node counts for the four M3-F4 arms over N book positions.

.DESCRIPTION
    The six-position tactical sweep (split-probe.txt) produced sign flips between
    depths: one position (sicilian, +214% at depth 12) carried the whole aggregate.
    A reduction/verification change has a long-tailed per-position node distribution,
    so six positions cannot resolve a 5% effect. This widens the corpus to the same
    UHO book the matches use and reports both the SUM and the per-position MEDIAN
    ratio, because the median is what a heavy tail makes the honest statistic.

    Arms are selected through the shipped UCI toggles, so this reproduces from the
    committed tree. (The numbers in results.md section 4 were first taken with temporary
    env-gated predicates, before the split toggles existed; the arms are identical.)

    DRIVER: real Process with live stdin -- `go depth` is asynchronous.
#>
[CmdletBinding()]
param(
    [string]$Engine = "target\release\manifold.exe",
    [int]$Depth = 12,
    [int]$Count = 24,
    [string]$Book = "tools\books\UHO_4060_v4.epd",
    [string]$OutFile = "experiments\MSN-S4-postlmr\book-nodes.txt"
)

$ErrorActionPreference = 'Stop'
$root = (Get-Location).Path
$enginePath = Join-Path $root $Engine

$fens = @()
foreach ($line in Get-Content (Join-Path $root $Book)) {
    $f = $line -split '\s+'
    if ($f.Count -lt 4) { continue }
    $fens += ($f[0..3] -join ' ') + ' 0 1'
    if ($fens.Count -ge $Count) { break }
}

$arms = @(
    @{ name = 'neither';    depth = $false; hist = $false },
    @{ name = 'depth-only'; depth = $true;  hist = $false },
    @{ name = 'hist-only';  depth = $false; hist = $true  },
    @{ name = 'both';       depth = $true;  hist = $true  }
)

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

function Start-Engine([hashtable]$Arm) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName               = $enginePath
    $psi.WorkingDirectory       = $root
    $psi.RedirectStandardInput  = $true
    $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute        = $false
    $p = [System.Diagnostics.Process]::Start($psi)
    $p.StandardInput.WriteLine('uci')
    Read-Until $p 'uciok' | Out-Null
    $p.StandardInput.WriteLine('setoption name Hash value 64')
    $p.StandardInput.WriteLine("setoption name UsePostLMRDepth value $($Arm.depth.ToString().ToLower())")
    $p.StandardInput.WriteLine("setoption name UsePostLMRContHist value $($Arm.hist.ToString().ToLower())")
    $p
}

function Invoke-Search([System.Diagnostics.Process]$p, [string]$Fen, [int]$D) {
    $w = $p.StandardInput
    # ucinewgame clears the tables, so every position is measured from the same state
    # in every arm -- otherwise the history a previous position left behind becomes an
    # uncontrolled variable that differs BETWEEN arms by construction.
    $w.WriteLine('ucinewgame')
    $w.WriteLine('isready')
    Read-Until $p 'readyok' | Out-Null
    $w.WriteLine("position fen $Fen")
    $w.WriteLine("go depth $D")
    $out = Read-Until $p '^bestmove'
    $last = $out | Where-Object { $_ -match "^info depth $D " } | Select-Object -Last 1
    if ($last -match ' nodes (\d+)') { [int64]$matches[1] } else { 0L }
}

$nodes = @{}
foreach ($arm in $arms) {
    $p = Start-Engine $arm
    $nodes[$arm.name] = @()
    foreach ($fen in $fens) { $nodes[$arm.name] += (Invoke-Search $p $fen $Depth) }
    $p.StandardInput.WriteLine('quit')
    $p.WaitForExit(20000) | Out-Null
}

function Median([double[]]$v) {
    $s = $v | Sort-Object
    if ($s.Count % 2 -eq 1) { $s[[int](($s.Count - 1) / 2)] }
    else { ($s[$s.Count / 2 - 1] + $s[$s.Count / 2]) / 2.0 }
}

$lines = @()
$lines += "M3-F4 fixed-depth book node counts -- $(Get-Date -Format s)"
$lines += "Engine: $enginePath   depth $Depth   $($fens.Count) positions from $Book"
$lines += ''
$lines += ('{0,-12} {1,14} {2,12} {3,14}' -f 'arm', 'total nodes', 'vs neither', 'median ratio')
$base = ($nodes['neither'] | Measure-Object -Sum).Sum
foreach ($arm in $arms) {
    $sum = ($nodes[$arm.name] | Measure-Object -Sum).Sum
    $ratios = 0..($fens.Count - 1) | ForEach-Object {
        if ($nodes['neither'][$_] -gt 0) { [double]$nodes[$arm.name][$_] / $nodes['neither'][$_] } else { 1.0 }
    }
    $lines += ('{0,-12} {1,14} {2,11:N2}% {3,14:N3}' -f $arm.name, $sum,
        (100.0 * ($sum - $base) / $base), (Median $ratios))
}
$lines += ''
$lines += 'per-position nodes:'
$lines += ('{0,-4} {1,12} {2,12} {3,12} {4,12}' -f '#', 'neither', 'depth-only', 'hist-only', 'both')
for ($i = 0; $i -lt $fens.Count; $i++) {
    $lines += ('{0,-4} {1,12} {2,12} {3,12} {4,12}' -f $i,
        $nodes['neither'][$i], $nodes['depth-only'][$i], $nodes['hist-only'][$i], $nodes['both'][$i])
}

$lines | ForEach-Object { Write-Output $_ }
$lines | Set-Content -LiteralPath $OutFile -Encoding utf8
Write-Output "wrote $OutFile"
