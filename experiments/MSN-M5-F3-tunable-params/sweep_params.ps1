# Per-parameter wiring sweep: bench each tunable at its min and its max.
#
# Purpose is attribution, not tuning. A spin whose min AND max both reproduce the
# shipped 41,588 is either dead wiring or genuinely unreachable on the bench suite,
# and the results doc has to say WHICH.
$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'
$exe = Join-Path $root 'target\release\manifold.exe'
$net = Join-Path $root 'nets\main.nnue'
$tmp = Join-Path $env:TEMP "mf-sweep-$PID"
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

# name,default,min,max harvested from the handshake so the sweep can never drift
# from what the engine actually advertises.
$inFile = Join-Path $tmp 'hs.txt'
"uci`nquit`n" | Out-File -FilePath $inFile -Encoding ascii -NoNewline
$handshake = & cmd.exe /c "`"$exe`" < `"$inFile`""
$specs = $handshake |
    Where-Object { $_ -match '^option name (\S+) type spin default (-?\d+) min (-?\d+) max (-?\d+)$' } |
    ForEach-Object {
        if ($_ -match '^option name (\S+) type spin default (-?\d+) min (-?\d+) max (-?\d+)$') {
            [pscustomobject]@{ Name = $Matches[1]; Default = [int]$Matches[2]; Min = [int]$Matches[3]; Max = [int]$Matches[4] }
        }
    } |
    Where-Object { $_.Name -notin @('Threads', 'Hash') }

function Bench([string]$setoptions) {
    $f = Join-Path $tmp 'in.txt'
    ("setoption name EvalFile value $net`n" + $setoptions + "bench`nquit`n") |
        Out-File -FilePath $f -Encoding ascii -NoNewline
    $out = & cmd.exe /c "`"$exe`" < `"$f`""
    [uint64](($out | Where-Object { $_ -match '^Nodes searched: (\d+)' }) -replace '^Nodes searched: ', '')
}

$shipped = Bench ''
Write-Output "shipped: $shipped"
Write-Output "name,min,minNodes,max,maxNodes"
foreach ($spec in $specs) {
    $atMin = Bench "setoption name $($spec.Name) value $($spec.Min)`n"
    $atMax = Bench "setoption name $($spec.Name) value $($spec.Max)`n"
    Write-Output ("{0},{1},{2},{3},{4}" -f $spec.Name, $spec.Min, $atMin, $spec.Max, $atMax)
}

Remove-Item -Recurse -Force $tmp
