# Manual UCI verification for M2-F3b (threat-discovery scan split).
#
# Process-based driver with blocking ReadLine: piped here-strings close stdin and abort
# `go movetime`, and add_OutputDataReceived / Task::Run both fail with "no Runspace available"
# inside a script file (library/user-testing.md).

param([string]$Exe = ".\target-f3b\release\manifold.exe")

$info = New-Object System.Diagnostics.ProcessStartInfo
$info.FileName = (Resolve-Path $Exe).Path
$info.RedirectStandardInput = $true
$info.RedirectStandardOutput = $true
$info.UseShellExecute = $false
$process = [System.Diagnostics.Process]::Start($info)

function Send([string]$command) {
    $process.StandardInput.WriteLine($command)
    $process.StandardInput.Flush()
    Write-Host "> $command"
}

function ReadUntil([string]$sentinel, [int]$timeoutSeconds = 120) {
    $deadline = (Get-Date).AddSeconds($timeoutSeconds)
    $last = $null
    while ((Get-Date) -lt $deadline) {
        $line = $process.StandardOutput.ReadLine()
        if ($null -eq $line) { break }
        # Keep the transcript readable: every sentinel line, but only the deepest info line.
        if ($line.StartsWith("info depth")) { $last = $line }
        else { Write-Host $line }
        if ($line.StartsWith($sentinel)) {
            if ($last) { Write-Host "  (deepest) $last" }
            return $line
        }
    }
    throw "timed out waiting for '$sentinel'"
}

Send "uci"
ReadUntil "uciok" | Out-Null
Send "isready"
ReadUntil "readyok" | Out-Null

# Startpos at fixed depth: the plain incremental discovery path.
Send "position startpos"
Send "go depth 18"
ReadUntil "bestmove" | Out-Null

# Endgame with long slider rays: the empty-square branch (discovered contacts through a
# vacated square) runs on almost every push here.
Send "position fen 8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1"
Send "go movetime 3000"
ReadUntil "bestmove" | Out-Null

# Kiwipete: dense middlegame, the occupied-square branch dominates.
Send "position fen r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1"
Send "go movetime 2000"
ReadUntil "bestmove" | Out-Null

# Chess960 castling: four affected squares per castle, several empty on one side of the move,
# driven through the real protocol with king-takes-rook notation.
Send "setoption name UCI_Chess960 value true"
Send "position fen 1rk1r3/8/8/8/8/8/8/1RK1R3 w EBeb - 0 1"
Send "go movetime 2000"
ReadUntil "bestmove" | Out-Null

$workingSet = [math]::Round((Get-Process -Id $process.Id).WorkingSet64 / 1MB, 1)
Write-Host "working set (MiB): $workingSet"

Send "quit"
$process.WaitForExit(15000) | Out-Null
Write-Host "exit code: $($process.ExitCode)"

