# Scans several positions/depths for fixed-depth thread dependence.
#
# The correction- and pawn-history tables size themselves as
# BASE_BUCKETS * nextPow2(threads) (history.rs), so the bucket MASK differs between
# Threads=1 and Threads=8 even when the helpers never search. A hash collision that
# happens at 512 buckets and not at 4096 changes the correction applied to a static
# eval, which changes the tree. This scan tests whether that coupling is pre-existing
# rather than introduced by the quiet-check widening.
param([Parameter(Mandatory = $true)][string]$Engine, [string[]]$Setup = @())
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'

$positions = @(
    'startpos moves e2e4 e7e5 g1f3',
    'startpos',
    'fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1',
    'fen 8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1',
    'fen r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10'
)

function Run([int]$threads, [string]$position, [int]$depth) {
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
    $p.StandardInput.WriteLine("position $position")
    $p.StandardInput.WriteLine("go depth $depth")
    $lines = @()
    while ($true) {
        $line = $p.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        if ($line -match 'multipv 1') { $lines += ($line -replace ' (nps|time) \d+', '') }
        if ($line -match '^bestmove') { $lines += $line; break }
    }
    $p.StandardInput.WriteLine('quit')
    $p.WaitForExit(15000) | Out-Null
    $lines
}

foreach ($position in $positions) {
    foreach ($depth in @(8, 9, 10)) {
        $one   = Run 1 $position $depth
        $eight = Run 8 $position $depth
        $verdict = if (($one -join "`n") -eq ($eight -join "`n")) { 'IDENTICAL' } else { 'DIFFERENT' }
        Write-Output ("depth {0}  {1,-9}  {2}" -f $depth, $verdict, $position)
    }
}
