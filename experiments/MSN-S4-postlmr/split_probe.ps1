<#
.SYNOPSIS
    Attributes the M3-F4 fixed-depth node cost to ONE of the two post-LMR sub-mechanisms.

.DESCRIPTION
    The feature was specified as a single package. It measured +26% / +30% nodes at
    fixed depths 12 / 14, so the package must be split before anything is matched. The
    release binary carries TEMPORARY instrumentation reading two environment variables:

      MF_DIAG_NO_DEPTH  -- disables the doDeeper/doShallower verification-depth band
      MF_DIAG_NO_HIST   -- disables the post-LMR continuation-history bonus

    Four arms: both off (= UsePostLMR off), each alone, both on. Fixed depth is the only
    valid comparison for a change that alters how much tree gets searched.

    DRIVER: real Process with live stdin. `go depth` is asynchronous and file-redirected
    stdin truncates it (library/user-testing.md, MSN-S2 driver note).
#>
[CmdletBinding()]
param(
    [string]$Engine = "target\release\manifold.exe",
    [int[]]$Depths = @(10, 12, 14),
    [string]$OutFile = "experiments\MSN-S4-postlmr\split-probe.txt"
)

$ErrorActionPreference = 'Stop'
$root = (Get-Location).Path
$enginePath = Join-Path $root $Engine

$positions = @(
    @{ name = 'startpos';   fen = 'startpos' },
    @{ name = 'kiwipete';   fen = 'fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1' },
    @{ name = 'italian';    fen = 'fen r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10' },
    @{ name = 'promo-race'; fen = 'fen rnbq1k1r/pp1Pbppp/2p5/8/2B5/8/PPP1NnPP/RNBQK2R w KQ - 1 8' },
    @{ name = 'chaos';      fen = 'fen r3k2r/Pppp1ppp/1b3nbN/nP6/BBP1P3/q4N2/Pp1P2PP/R2Q1RK1 w kq - 0 1' },
    @{ name = 'sicilian';   fen = 'fen 2kr3r/pp1q1ppp/5n2/1Nb5/2Pp1B2/7Q/P4PPP/1R3RK1 w - - 0 1' }
)

$arms = @(
    @{ name = 'neither';    noDepth = $true;  noHist = $true  },
    @{ name = 'depth-only'; noDepth = $false; noHist = $true  },
    @{ name = 'hist-only';  noDepth = $true;  noHist = $false },
    @{ name = 'both';       noDepth = $false; noHist = $false }
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

function Invoke-Search {
    param([hashtable]$Arm, [string]$Position, [int]$Depth)

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName               = $enginePath
    $psi.WorkingDirectory       = $root
    $psi.RedirectStandardInput  = $true
    $psi.RedirectStandardOutput = $true
    $psi.UseShellExecute        = $false
    if ($Arm.noDepth) { $psi.EnvironmentVariables['MF_DIAG_NO_DEPTH'] = '1' }
    if ($Arm.noHist)  { $psi.EnvironmentVariables['MF_DIAG_NO_HIST']  = '1' }
    $p = [System.Diagnostics.Process]::Start($psi)

    $w = $p.StandardInput
    $w.WriteLine('uci')
    Read-Until $p 'uciok' | Out-Null
    $w.WriteLine('setoption name Hash value 64')
    $w.WriteLine('ucinewgame')
    $w.WriteLine('isready')
    Read-Until $p 'readyok' | Out-Null
    $w.WriteLine("position $Position")
    $w.WriteLine("go depth $Depth")
    $out = Read-Until $p '^bestmove'
    $w.WriteLine('quit')
    $p.WaitForExit(20000) | Out-Null

    $last = $out | Where-Object { $_ -match "^info depth $Depth " } | Select-Object -Last 1
    $best = $out | Where-Object { $_ -match '^bestmove ' } | Select-Object -Last 1
    $nodes = 0
    if ($last -match ' nodes (\d+)') { $nodes = [int64]$matches[1] }
    $move = ''
    if ($best -match '^bestmove (\S+)') { $move = $matches[1] }
    [pscustomobject]@{ Nodes = $nodes; Move = $move }
}

$lines = @()
$lines += "M3-F4 post-LMR sub-mechanism attribution -- $(Get-Date -Format s)"
$lines += "Engine: $enginePath (temporary MF_DIAG_* instrumentation)"
$lines += ''

foreach ($depth in $Depths) {
    $lines += "== depth $depth =="
    $lines += ('{0,-12} {1,12} {2,12} {3,12} {4,12}' -f 'position', 'neither', 'depth-only', 'hist-only', 'both')
    $totals = @{}
    foreach ($arm in $arms) { $totals[$arm.name] = 0L }
    foreach ($pos in $positions) {
        $row = @{}
        foreach ($arm in $arms) {
            $r = Invoke-Search -Arm $arm -Position $pos.fen -Depth $depth
            $row[$arm.name] = $r
            $totals[$arm.name] += $r.Nodes
        }
        $lines += ('{0,-12} {1,12} {2,12} {3,12} {4,12}' -f $pos.name,
            $row['neither'].Nodes, $row['depth-only'].Nodes, $row['hist-only'].Nodes, $row['both'].Nodes)
    }
    $base = $totals['neither']
    $lines += ('{0,-12} {1,12} {2,12} {3,12} {4,12}' -f 'TOTAL',
        $totals['neither'], $totals['depth-only'], $totals['hist-only'], $totals['both'])
    $lines += ('{0,-12} {1,12} {2,11:N1}% {3,11:N1}% {4,11:N1}%' -f 'vs neither', '',
        (100.0 * ($totals['depth-only'] - $base) / $base),
        (100.0 * ($totals['hist-only']  - $base) / $base),
        (100.0 * ($totals['both']       - $base) / $base))
    $lines += ''
}

$lines | ForEach-Object { Write-Output $_ }
$lines | Set-Content -LiteralPath $OutFile -Encoding utf8
Write-Output "wrote $OutFile"
