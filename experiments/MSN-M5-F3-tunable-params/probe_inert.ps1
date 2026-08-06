# Wiring probe for the four parameters that do not move the bench-depth-7 signature.
#
# Bench is one fixed depth on six positions, so a parameter can be correctly wired and
# still be unreachable there. This drives a DEEPER fixed-depth search (and, for the
# post-LMR bonus, its owning toggle) so the sweep's five inert rows are attributed to a
# reachability reason rather than left as "possibly dead wiring".
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'
$exe = Join-Path $root 'target\release\manifold.exe'
$fen = 'r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1'

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $exe
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = $root
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput

function ReadUntil([string]$prefix) {
    $lines = @()
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        $lines += $line
        if ($line.StartsWith($prefix)) { break }
    }
    $lines
}

function Nodes([string]$setoptions, [int]$depth) {
    $w.WriteLine('ucinewgame')
    foreach ($line in $setoptions -split "`n" | Where-Object { $_ }) { $w.WriteLine($line) }
    $w.WriteLine('isready'); ReadUntil 'readyok' | Out-Null
    $w.WriteLine("position fen $fen")
    $w.WriteLine("go depth $depth")
    $out = ReadUntil 'bestmove'
    $last = $out | Where-Object { $_ -match ' nodes (\d+) ' } | Select-Object -Last 1
    if ($last -match ' nodes (\d+) ') { [uint64]$Matches[1] } else { 0 }
}

$w.WriteLine('uci'); ReadUntil 'uciok' | Out-Null

$baseline = Nodes '' 12
Write-Output "depth-12 baseline: $baseline"

foreach ($case in @(
        @{ Name = 'NmpEvalReductionDivisor=50'; Set = "setoption name NmpEvalReductionDivisor value 50" },
        @{ Name = 'NmpEvalReductionMax=0'; Set = "setoption name NmpEvalReductionMax value 0" },
        @{ Name = 'SingularBetaTtPvBonus=200'; Set = "setoption name SingularBetaTtPvBonus value 200" }
    )) {
    $reset = "setoption name NmpEvalReductionDivisor value 200`nsetoption name NmpEvalReductionMax value 3`nsetoption name SingularBetaTtPvBonus value 66"
    $n = Nodes ($reset + "`n" + $case.Set) 12
    Write-Output ("{0}: {1} (delta {2})" -f $case.Name, $n, ([int64]$n - [int64]$baseline))
}

# The post-LMR continuation bonus is gated by a toggle that ships OFF, so it is measured
# with that toggle ON -- otherwise the site that reads it is unreachable by construction.
$reset = "setoption name NmpEvalReductionDivisor value 200`nsetoption name NmpEvalReductionMax value 3`nsetoption name SingularBetaTtPvBonus value 66"
$onDefault = Nodes ($reset + "`nsetoption name UsePostLMRContHist value true") 12
$onZero = Nodes ($reset + "`nsetoption name UsePostLMRContHist value true`nsetoption name PostLmrContinuationBonus value 0") 12
Write-Output "UsePostLMRContHist=true, bonus 1334: $onDefault"
Write-Output "UsePostLMRContHist=true, bonus 0:    $onZero (delta $([int64]$onZero - [int64]$onDefault))"

$w.WriteLine('quit')
$p.WaitForExit(5000) | Out-Null
