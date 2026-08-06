<#
.SYNOPSIS
    Live UCI session against the M3-F4 build: option list, timed search, both toggles.

.DESCRIPTION
    Verifies through the surface a GUI actually drives (A-HASH-003 shape): uci/uciok,
    isready/readyok, a timed `go movetime` producing well-formed info lines with
    depth/nps/hashfull and a legal bestmove, and that each post-LMR toggle changes the
    tree in a real session.

    DRIVER: real System.Diagnostics.Process with a live stdin writer and blocking
    ReadLine(). File-redirected stdin delivers `quit` immediately and aborts
    `go movetime` (library/user-testing.md).
#>
[CmdletBinding()]
param(
    [string]$Engine = "target\release\manifold.exe",
    [string]$OutFile = "experiments\MSN-S4-postlmr\uci-probe-transcript.txt"
)

$ErrorActionPreference = 'Stop'
$root = (Get-Location).Path
$enginePath = Join-Path $root $Engine
$transcript = @()

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

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName               = $enginePath
$psi.WorkingDirectory       = $root
$psi.RedirectStandardInput  = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute        = $false
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput

$w.WriteLine('uci')
$uci = Read-Until $p '^uciok'
$transcript += '--- uci ---'
$transcript += ($uci | Where-Object { $_ -match 'PostLMR|^uciok|^id name' })

$w.WriteLine('setoption name Hash value 64')
$w.WriteLine('isready')
$transcript += '--- isready ---'
$transcript += (Read-Until $p '^readyok')

$fen = 'fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1'
$w.WriteLine('ucinewgame')
$w.WriteLine('isready')
Read-Until $p '^readyok' | Out-Null
$w.WriteLine("position $fen")
$w.WriteLine('go movetime 2000')
$timed = Read-Until $p '^bestmove'
$transcript += '--- go movetime 2000 (kiwipete, shipped defaults) ---'
$transcript += ($timed | Select-Object -Last 4)

$w.WriteLine('quit')
$p.WaitForExit(10000) | Out-Null

# Toggle arms: fixed depth, one engine process each, so the tables start clean.
function Invoke-Depth([string[]]$Setup, [int]$Depth) {
    $psi2 = New-Object System.Diagnostics.ProcessStartInfo
    $psi2.FileName               = $enginePath
    $psi2.WorkingDirectory       = $root
    $psi2.RedirectStandardInput  = $true
    $psi2.RedirectStandardOutput = $true
    $psi2.UseShellExecute        = $false
    $q = [System.Diagnostics.Process]::Start($psi2)
    $qw = $q.StandardInput
    $qw.WriteLine('uci')
    Read-Until $q '^uciok' | Out-Null
    $qw.WriteLine('setoption name Hash value 64')
    foreach ($s in $Setup) { $qw.WriteLine($s) }
    $qw.WriteLine('ucinewgame')
    $qw.WriteLine('isready')
    Read-Until $q '^readyok' | Out-Null
    $qw.WriteLine("position $fen")
    $qw.WriteLine("go depth $Depth")
    $out = Read-Until $q '^bestmove'
    $qw.WriteLine('quit')
    $q.WaitForExit(20000) | Out-Null
    $last = $out | Where-Object { $_ -match "^info depth $Depth " } | Select-Object -Last 1
    $best = $out | Where-Object { $_ -match '^bestmove ' } | Select-Object -Last 1
    "$last`n$best"
}

$transcript += ''
$transcript += '--- go depth 14, toggle arms ---'
$transcript += 'shipped (UsePostLMRDepth=true, UsePostLMRContHist=false):'
$transcript += (Invoke-Depth @() 14)
$transcript += 'depth band off:'
$transcript += (Invoke-Depth @('setoption name UsePostLMRDepth value false') 14)
$transcript += 'conthist bonus on:'
$transcript += (Invoke-Depth @('setoption name UsePostLMRContHist value true') 14)

$transcript | ForEach-Object { Write-Output $_ }
$transcript | Set-Content -LiteralPath $OutFile -Encoding utf8
Write-Output "wrote $OutFile"
