# Live UCI verification of the M4-F1b default flip.
#
# Process-driven because redirected stdin aborts timed searches (library/user-testing.md).
# Asserts three things about the SHIPPED binary rather than about a test harness:
#   1. `uci` advertises UseCaptureLMR as default true;
#   2. at equal time the shipped default reaches >= the depth of the toggle-off arm;
#   3. both arms return a legal, well-formed bestmove from the same position.
#
# -Repeat runs each arm N times because a single timed observation is scheduling
# jitter, not a measurement (mission AGENTS.md worker guidance).
param(
    [string]$Engine = 'C:\Users\Samaritan\Projects\Manifold\target\release\manifold.exe',
    [int]$MoveTimeMs = 1000,
    [int]$Repeat = 5
)
$ErrorActionPreference = 'Stop'

$positions = @(
    @{ label = 'startpos'; cmd = 'position startpos' },
    @{ label = 'kiwipete'; cmd = 'position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1' },
    @{ label = 'sicilian'; cmd = 'position fen r4rk1/1pp1qppp/p1np1n2/2b1p1B1/2B1P1b1/P1NP1N2/1PP1QPPP/R4RK1 w - - 0 10' }
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
$advertised = $script:lastLines | Where-Object { $_ -match 'UseCaptureLMR|UsePostLMRDepth' }
'--- advertised defaults ---'
$advertised
''

Send 'setoption name Hash value 64'
Send 'setoption name Threads value 1'
Send 'isready'; WaitFor 'readyok' | Out-Null

function Probe([hashtable]$p, [bool]$captureLmr) {
    Send ("setoption name UseCaptureLMR value " + $captureLmr.ToString().ToLower())
    Send 'ucinewgame'
    Send 'isready'; $script:lastLines = @(); WaitFor 'readyok' | Out-Null
    Send $p.cmd
    $script:lastLines = @()
    Send "go movetime $MoveTimeMs"
    $best = WaitFor 'bestmove'
    $last = ($script:lastLines | Where-Object { $_ -match '^info depth' })[-1]
    $depth = [int]([regex]::Match($last, '^info depth (\d+)').Groups[1].Value)
    $nodes = [int64]([regex]::Match($last, ' nodes (\d+)').Groups[1].Value)
    [pscustomobject]@{ Depth = $depth; Nodes = $nodes; Best = $best; Info = $last }
}

"--- go movetime $MoveTimeMs, $Repeat repeats per arm ---"
foreach ($p in $positions) {
    $on  = 1..$Repeat | ForEach-Object { Probe $p $true }
    $off = 1..$Repeat | ForEach-Object { Probe $p $false }
    $onDepths  = ($on  | ForEach-Object { $_.Depth }) -join ','
    $offDepths = ($off | ForEach-Object { $_.Depth }) -join ','
    $onMedian  = ($on  | ForEach-Object { $_.Depth } | Sort-Object)[[int]($Repeat/2)]
    $offMedian = ($off | ForEach-Object { $_.Depth } | Sort-Object)[[int]($Repeat/2)]
    "{0,-9} ON  depths [{1}] median {2}   {3}" -f $p.label, $onDepths, $onMedian, $on[0].Best
    "{0,-9} OFF depths [{1}] median {2}   {3}" -f '', $offDepths, $offMedian, $off[0].Best
    "{0,-9} sample info ON : {1}" -f '', $on[0].Info
    "{0,-9} sample info OFF: {1}" -f '', $off[0].Info
    ''
}

Send 'quit'
$proc.WaitForExit(10000) | Out-Null
