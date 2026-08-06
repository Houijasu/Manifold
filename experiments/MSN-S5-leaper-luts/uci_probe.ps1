# Process-driven UCI probe: piped/redirected stdin aborts timed searches, so drive a
# real System.Diagnostics.Process with a live stdin writer and blocking ReadLine().
param(
    [string]$Engine = 'C:\Users\Samaritan\Projects\Manifold\target\release\manifold.exe',
    [int]$MoveTimeMs = 1000
)
$ErrorActionPreference = 'Stop'

$positions = @(
    @{ label = 'startpos'; cmd = 'position startpos' },
    @{ label = 'kiwipete'; cmd = 'position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1' }
)

$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = $Engine
$info.RedirectStandardInput = $true
$info.RedirectStandardOutput = $true
$info.UseShellExecute = $false
$proc = [System.Diagnostics.Process]::Start($info)

function Send([string]$line) { $proc.StandardInput.WriteLine($line); $proc.StandardInput.Flush() }
function WaitFor([string]$prefix) {
    while ($true) {
        $line = $proc.StandardOutput.ReadLine()
        if ($null -eq $line) { throw "engine closed while waiting for $prefix" }
        $script:lastLines += $line
        if ($line.StartsWith($prefix)) { return $line }
    }
}

Send 'uci'; $script:lastLines = @(); WaitFor 'uciok' | Out-Null
Send 'setoption name Hash value 64'
Send 'setoption name Threads value 1'
Send 'isready'; WaitFor 'readyok' | Out-Null

foreach ($p in $positions) {
    Send 'ucinewgame'
    Send 'isready'; $script:lastLines = @(); WaitFor 'readyok' | Out-Null
    Send $p.cmd
    $script:lastLines = @()
    Send "go movetime $MoveTimeMs"
    $best = WaitFor 'bestmove'
    $last = ($script:lastLines | Where-Object { $_ -match '^info depth' })[-1]
    "{0,-9} {1}" -f $p.label, $last
    "{0,-9} {1}" -f '', $best
}

Send 'quit'
$proc.WaitForExit(10000) | Out-Null
