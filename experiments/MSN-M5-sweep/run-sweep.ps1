<#
    M5-F2 toggle smoke sweep.

    For every `type check` toggle in the live UCI handshake of the release build, run ONE
    10-game match (-Rounds 5) of that toggle FLIPPED from its shipped default against the
    SAME binary at defaults. Same binary both arms, one variable, fixed seed across every
    sweep.

    This is a FUNCTIONAL smoke test, not a strength measurement. n=10 has roughly +/-200
    Elo of error; it can only see crashes, forfeits, illegal moves and catastrophic
    breakage. Elo point estimates from these matches are not evidence.

    harness/run_match.ps1 is invoked with `&` and named parameters, NOT via
    `Start-Process -ArgumentList @(...)`: Start-Process joins an argument array with
    spaces without re-quoting, so the mandatory multi-word `-Purpose` string gets
    re-split and its trailing words bind onto whatever parameter comes next. Calling the
    script directly binds each parameter by name, and the driver's own `exit 3` still
    surfaces here as $LASTEXITCODE.

    The sweep STOPS at the first non-zero exit: per the feature spec, a flipped arm that
    crashes, forfeits, or plays an illegal move is a real defect and must be escalated
    rather than swept past.
#>
[CmdletBinding()]
param(
    [string]$Engine = 'C:\Users\Samaritan\Projects\Manifold\target\release\manifold.exe',
    [string]$SweepRoot = 'C:\Users\Samaritan\Projects\Manifold\experiments\MSN-M5-sweep',
    [int]$Seed = 20260807,
    [int]$Rounds = 5,
    [string]$TC = '8+0.08',
    [int]$Hash = 64
)

$ErrorActionPreference = 'Stop'
$root = 'C:\Users\Samaritan\Projects\Manifold'
$runMatch = Join-Path $root 'harness\run_match.ps1'

# name -> shipped default, read off the live handshake (uci-handshake.txt).
# UCI_Chess960 is excluded: it is a rules toggle, not a search toggle, and flipping it
# against a standard-chess opening book compares two different games.
$toggles = [ordered]@{
    'UseNMP'                 = $true
    'UseRFP'                 = $true
    'UseRazoring'            = $true
    'UseLMR'                 = $true
    'UseLMP'                 = $true
    'UseFutility'            = $true
    'UseSEEPruning'          = $true
    'UseQSearchTT'           = $true
    'UseQSearchDeltaPruning' = $true
    'UseQSearchChecks'       = $false
    'UseCaptureLMR'          = $true
    'UsePostLMRDepth'        = $true
    'UsePostLMRContHist'     = $false
    'UseSingularExt'         = $true
    'UseCheckExt'            = $true
    'UseMultiCut'            = $true
    'UseIIR'                 = $true
    'UseProbCut'             = $true
    'UseButterflyHistory'    = $true
    'UseCaptureHistory'      = $true
    'UsePawnHistory'         = $false
    'UseContHistory'         = $true
    'UseHistoryPruning'      = $false
    'UseCorrHistory'         = $true
    'UseCorrHistPawn'        = $true
    'UseCorrHistMinor'       = $true
    'UseCorrHistMajor'       = $false
    'UseCorrHistMaterial'    = $false
    'UseCorrHistCont'        = $true
    'UseTimeEffort'          = $false
}

$ledger = Join-Path $SweepRoot 'sweep-ledger.tsv'
"toggle`tdefault`tflipped`texit`twall_s" | Out-File -FilePath $ledger -Encoding utf8

$index = 0
$total = $toggles.Count
foreach ($name in $toggles.Keys) {
    $index++
    $default = $toggles[$name]
    $flipped = (-not $default).ToString().ToLowerInvariant()
    $defaultStr = $default.ToString().ToLowerInvariant()
    $outDir = Join-Path $SweepRoot $name

    Write-Host ''
    Write-Host "=== [$index/$total] $name  default=$defaultStr  flipped arm=$flipped ===" -ForegroundColor Magenta

    $purpose = "M5-F2 functional smoke: $name flipped to $flipped vs shipped defaults " +
               '(n=10, functional check only, NOT a strength measurement)'

    $started = Get-Date
    & $runMatch -OutDir $outDir -Purpose $purpose `
        -AName 'flipped' -ACmd $Engine -AOptions "option.$name=$flipped" `
        -BName 'default' -BCmd $Engine `
        -TC $TC -Hash $Hash -Rounds $Rounds -Seed $Seed
    $exit = $LASTEXITCODE
    $wall = [int]((Get-Date) - $started).TotalSeconds

    "$name`t$defaultStr`t$flipped`t$exit`t$wall" | Out-File -Append -FilePath $ledger -Encoding utf8
    Write-Host "--- $name exit=$exit wall=${wall}s ---" -ForegroundColor Magenta

    if ($exit -ne 0) {
        Write-Host ''
        Write-Host "SWEEP STOPPED: $name exited $exit. See $outDir." -ForegroundColor Red
        "SWEEP STOPPED at $name (exit $exit) after $index of $total toggles." |
            Out-File -Append -FilePath $ledger -Encoding utf8
        exit $exit
    }

    $stray = Get-Process manifold, fastchess -ErrorAction SilentlyContinue
    if ($stray) {
        Write-Host "[warn] stray engine processes after ${name}: $($stray.Id -join ',')" -ForegroundColor Yellow
    }
}

Write-Host ''
Write-Host "SWEEP COMPLETE: $total/$total toggles, every match exit 0." -ForegroundColor Green
"SWEEP COMPLETE: $total toggles, all exit 0." | Out-File -Append -FilePath $ledger -Encoding utf8
exit 0
