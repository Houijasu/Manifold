param(
    [string]$Fen = "7k/6Q1/6K1/8/8/8/8/8 w - - 0 1",
    [int]$ThinkMs = 6000,
    [int]$Threads = 1,
    [string]$Go = "go infinite",
    [int]$StopWaitMs = 30000
)

# Reproduces the user-reported "infinite analysis iterates without bound and cannot be
# stopped" scenario and measures how long `stop` takes to answer with `bestmove`.
#
# Engine stdout is redirected to a file rather than a pipe: a full OS pipe blocks the
# engine mid-search and is indistinguishable from a genuine hang. The transcript is read
# back with FileShare.ReadWrite because cmd.exe holds the redirect handle for the whole
# session.
$exe = Join-Path $PWD "target\release\manifold.exe"
$out = Join-Path ([System.IO.Path]::GetTempPath()) ("mf-probe-" + [guid]::NewGuid().ToString("N") + ".txt")

$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = "cmd.exe"
$psi.Arguments = "/c `"`"$exe`" > `"$out`"`""
$psi.RedirectStandardInput = $true
$psi.UseShellExecute = $false
$psi.WorkingDirectory = "$PWD"
$p = [System.Diagnostics.Process]::Start($psi)
$w = $p.StandardInput

function Transcript() {
    if (-not (Test-Path $out)) { return @() }
    $fs = [System.IO.File]::Open($out, 'Open', 'Read', 'ReadWrite')
    $sr = New-Object System.IO.StreamReader($fs)
    $text = $sr.ReadToEnd()
    $sr.Dispose(); $fs.Dispose()
    return ($text -split "`r?`n")
}

function MaxDepth([string[]]$lines) {
    $max = 0
    foreach ($line in $lines) {
        if ($line -match '^info depth (\d+)') {
            $d = [int]$Matches[1]
            if ($d -gt $max) { $max = $d }
        }
    }
    return $max
}

$w.WriteLine('uci')
if ($Threads -ne 1) { $w.WriteLine("setoption name Threads value $Threads") }
$w.WriteLine('isready')
Start-Sleep -Milliseconds 4000

$w.WriteLine("position fen $Fen")
$w.WriteLine($Go)
Start-Sleep -Milliseconds $ThinkMs

$lines = Transcript
$bytesDuringThink = (Get-Item $out).Length
Write-Output "max depth after $ThinkMs ms of '$Go': $(MaxDepth $lines) (stdout bytes: $bytesDuringThink)"
$premature = $lines | Where-Object { $_ -like 'bestmove*' } | Select-Object -First 1
if ($premature) { Write-Output "WARNING: answered before stop: $premature" }

$sw = [System.Diagnostics.Stopwatch]::StartNew()
$w.WriteLine('stop')
$best = $null
$deadline = [DateTime]::UtcNow.AddMilliseconds($StopWaitMs)
while ([DateTime]::UtcNow -lt $deadline) {
    Start-Sleep -Milliseconds 20
    $best = (Transcript) | Where-Object { $_ -like 'bestmove*' } | Select-Object -First 1
    if ($best) { break }
}
$sw.Stop()
Write-Output "stop -> bestmove latency: $($sw.ElapsedMilliseconds) ms; line: $best"

$final = Transcript
Write-Output "max depth overall: $(MaxDepth $final)"
Write-Output "stdout bytes after stop: $((Get-Item $out).Length)"

$w.WriteLine('quit')
if (-not $p.WaitForExit(10000)) { Write-Output "engine did not exit on quit; killing"; $p.Kill() }
Write-Output "exited: $($p.HasExited); transcript: $out"
