<#
.SYNOPSIS
    Reusable, guard-railed fastchess match driver for Manifold.

.DESCRIPTION
    Mission AGENTS.md 4.451 records a ~600 Elo measurement artifact that was produced by a
    single misplaced `-use-affinity` flag, and the ONLY thing that distinguished it from a
    genuine time-manager bug was a per-player "loses on time" forfeit count that a human
    happened to read off the console. A comment in a script is not enough. This driver
    ENFORCES both rules mechanically:

      RULE 1 (affinity)  -- Threads=1 on BOTH engines  => -use-affinity REQUIRED, concurrency 8.
                            Any engine with Threads>1  => -use-affinity FORBIDDEN, concurrency 1.
                            Violating either direction invalidates the measurement, so the
                            driver REFUSES TO RUN rather than producing a number.

      RULE 2 (forfeits)  -- After the match, per-player time-forfeit and crash counts are
                            parsed from the PGN (authoritative, per-game) and cross-checked
                            against the fastchess console summary. A non-zero count for any
                            engine ABORTS LOUDLY with exit code 3, unless that engine was
                            explicitly named in -ForfeitsAllowedFor (which still prints a
                            prominent warning and records the counts in run-metadata.txt).

    It also writes the mandatory AGENTS.md 4.7 `run-metadata.txt` provenance file. A match
    without one is not admissible evidence.

.EXAMPLE
    # Single-threaded SPRT (affinity on, concurrency 8 -- enforced automatically)
    .\harness\run_match.ps1 `
        -OutDir experiments\M4-cumulative `
        -Purpose 'Cumulative M4 vs M3 SPRT' `
        -AName M4 -ACmd .\target\release\manifold.exe `
        -BName M3 -BCmd .\baselines\M3\manifold.exe `
        -TC '8+0.08' -Rounds 2000 -Seed 20260901 `
        -Sprt 'elo0=0 elo1=5 alpha=0.05 beta=0.05'

.EXAMPLE
    # Multi-thread match (affinity refused, concurrency forced to 1 -- enforced automatically)
    .\harness\run_match.ps1 -OutDir experiments\smp -Purpose '8T vs 1T equal time' `
        -AName T8 -ACmd .\target\release\manifold.exe -AThreads 8 `
        -BName T1 -BCmd .\target\release\manifold.exe -BThreads 1 `
        -TC '10+0.1' -Hash 128 -Rounds 150 -Seed 20260902
#>
[CmdletBinding()]
param(
    # --- output / provenance -------------------------------------------------
    [Parameter(Mandatory = $true)][string]$OutDir,
    [Parameter(Mandatory = $true)][string]$Purpose,

    # --- engine A ------------------------------------------------------------
    [Parameter(Mandatory = $true)][string]$AName,
    [Parameter(Mandatory = $true)][string]$ACmd,
    [int]$AThreads = 1,
    [string[]]$AOptions = @(),
    [int]$ANodes = 0,

    # --- engine B ------------------------------------------------------------
    [Parameter(Mandatory = $true)][string]$BName,
    [Parameter(Mandatory = $true)][string]$BCmd,
    [int]$BThreads = 1,
    [string[]]$BOptions = @(),
    [int]$BNodes = 0,

    # --- match parameters ----------------------------------------------------
    [string]$TC = '8+0.08',
    [int]$Hash = 64,
    [int]$Rounds = 500,
    [int]$Seed = 0,
    [string]$Book = '',
    [string]$Sprt = '',
    [int]$RatingInterval = 50,

    # --- guardrail escape hatches (used sparingly, always recorded) -----------
    # Engines whose forfeits/crashes are RECORDED but do not abort. Used only for
    # third-party opponents (A-EONEGO-001 counts Eonego-side failures separately).
    [string[]]$ForfeitsAllowedFor = @(),
    [int]$Concurrency = 0
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'provenance.ps1')
$root = Get-ManifoldRepositoryRoot
$fc = Join-Path $root 'tools\fastchess\fastchess.exe'
if ([string]::IsNullOrWhiteSpace($Book)) {
    $Book = Join-Path $root 'tools\books\UHO_4060_v4.epd'
}

function Fail([string]$msg, [int]$code) {
    Write-Host ''
    Write-Host '################################################################' -ForegroundColor Red
    Write-Host "## HARNESS SELF-CHECK FAILED" -ForegroundColor Red
    Write-Host "## $msg" -ForegroundColor Red
    Write-Host '################################################################' -ForegroundColor Red
    exit $code
}

# ---------------------------------------------------------------------------
# RULE 1 -- affinity/concurrency, derived from thread counts and NOT overridable.
# ---------------------------------------------------------------------------
$maxThreads = [Math]::Max($AThreads, $BThreads)
if ($AThreads -lt 1 -or $BThreads -lt 1) { Fail "Thread counts must be >= 1 (got A=$AThreads B=$BThreads)." 2 }

if ($maxThreads -gt 1) {
    $useAffinity     = $false
    $requiredConcurr = 1
    $affinityReason  = "an engine runs Threads>1 (A=$AThreads B=$BThreads); AGENTS.md 4.451 forbids -use-affinity here"
} else {
    $useAffinity     = $true
    $requiredConcurr = 8
    $affinityReason  = 'both engines run Threads=1; AGENTS.md 4.451 makes -use-affinity mandatory'
}

if ($Concurrency -ne 0 -and $Concurrency -ne $requiredConcurr) {
    Fail ("Refusing to run: -Concurrency $Concurrency contradicts the mandated value $requiredConcurr, because $affinityReason. " +
          'A pinned multi-thread run manufactured a ~600 Elo artifact and 69 forfeits in 140 games; ' +
          'an unpinned single-thread run invalidates every M1-M5 SPRT. Fix the thread counts, not this flag.') 2
}
$Concurrency = $requiredConcurr

Write-Host "[guardrail] affinity=$useAffinity concurrency=$Concurrency -- $affinityReason" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# RULE 3 -- memory pre-flight. Same refusal pattern as RULE 1, for the same reason.
#
# M1-F2 established that a match whose engines do not fit in free physical memory PAGES,
# and that the symptom of paging is engines losing on time -- the exact signal RULE 2
# reserves for distinguishing a broken harness config from a genuine engine defect. Such a
# run is inadmissible whatever it prints, so the driver refuses to produce the number.
#
# The process count is the whole point: fastchess runs `-concurrency` GAMES at once and each
# game holds TWO engine processes, so a 1T match at the mandated concurrency 8 has SIXTEEN
# engines alive, each with its own Hash. That is why a Hash that looks harmless in isolation
# pages a 1T match (M1-F2 measured 4096 needing 65.7 GB against ~19.7 GB free). To exercise a
# large Hash, run at Threads>=2, which selects concurrency 1 and therefore two processes.
# ---------------------------------------------------------------------------
$engineProcesses = 2 * $Concurrency
$requiredMib     = $engineProcesses * $Hash
$freeMib         = [int]((Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory / 1024)
$budgetMib       = [int]($freeMib * 0.70)

Write-Host ("[guardrail] memory: {0} engine processes x Hash {1} MiB = {2} MiB required; free {3} MiB, budget {4} MiB (70%)" `
            -f $engineProcesses, $Hash, $requiredMib, $freeMib, $budgetMib) -ForegroundColor Cyan

if ($requiredMib -gt $budgetMib) {
    Fail ("Refusing to run: this match needs $requiredMib MiB of Hash " +
          "($engineProcesses engine processes = 2 x concurrency $Concurrency, each at Hash $Hash MiB) " +
          "but only $budgetMib MiB is budgetable (70% of $freeMib MiB free physical memory). " +
          'It would page, and paging shows up as engines losing on time -- which is the one signal ' +
          'AGENTS.md 4.451 reserves for a real defect, so the result would be uninterpretable ' +
          "either way. Lower -Hash to at most $([Math]::Floor($budgetMib / $engineProcesses)) MiB, " +
          'or run the large Hash at -AThreads 2 -BThreads 2 (concurrency 1 = 2 processes).') 2
}

# ---------------------------------------------------------------------------
# Resolve paths and build the command line.
# ---------------------------------------------------------------------------
if (-not (Test-Path $fc))    { Fail "fastchess not found at $fc" 2 }
if (-not (Test-Path $ACmd))  { Fail "engine A not found at $ACmd" 2 }
if (-not (Test-Path $BCmd))  { Fail "engine B not found at $BCmd" 2 }
if (-not (Test-Path $Book))  { Fail "opening book not found at $Book" 2 }

$ACmd = (Resolve-Path $ACmd).Path
$BCmd = (Resolve-Path $BCmd).Path
$Book = (Resolve-Path $Book).Path
if (-not (Test-Path $OutDir)) { New-Item -ItemType Directory -Path $OutDir -Force | Out-Null }
$OutDir = (Resolve-Path $OutDir).Path

if ($Seed -eq 0) { $Seed = Get-Random -Minimum 10000000 -Maximum 99999999 }

$pgn      = Join-Path $OutDir 'games.pgn'
$console  = Join-Path $OutDir 'console.txt'
$fclog    = Join-Path $OutDir 'fastchess.log'
$metaPath = Join-Path $OutDir 'run-metadata.txt'

$engineA = @("cmd=$ACmd", "name=$AName", "option.Hash=$Hash", "option.Threads=$AThreads") + $AOptions
$engineB = @("cmd=$BCmd", "name=$BName", "option.Hash=$Hash", "option.Threads=$BThreads") + $BOptions
if ($ANodes -gt 0) { $engineA += "nodes=$ANodes" }
if ($BNodes -gt 0) { $engineB += "nodes=$BNodes" }

$args = @('-engine') + $engineA + @('-engine') + $engineB +
        @('-each', 'proto=uci', "tc=$TC",
          '-openings', "file=$Book", 'format=epd', 'order=random',
          '-repeat', '-games', '2', '-rounds', "$Rounds",
          '-concurrency', "$Concurrency", '-srand', "$Seed",
          '-report', 'penta=true', '-ratinginterval', "$RatingInterval",
          '-pgnout', "file=$pgn", 'append=false',
          '-log', "file=$fclog", 'level=warn', 'append=false')
if ($useAffinity) { $args += '-use-affinity' }
if ($Sprt)        { $args += @('-sprt') + ($Sprt -split '\s+') }

$cmdLine = "$fc " + ($args -join ' ')

# ---------------------------------------------------------------------------
# AGENTS.md 4.7 provenance -- written BEFORE the run so a killed run still has it.
# ---------------------------------------------------------------------------
$driverCommit = Get-ManifoldHeadCommit
$sourceA = Get-BinarySourceAttestation $ACmd
$sourceB = Get-BinarySourceAttestation $BCmd
$shaA = (Get-FileHash -Algorithm SHA256 $ACmd).Hash
$shaB = (Get-FileHash -Algorithm SHA256 $BCmd).Hash
$provenanceMetadata = Format-ManifoldMatchProvenanceMetadata `
    -DriverCommit $driverCommit `
    -AName $AName -ACmd $ACmd -SourceA $sourceA -ShaA $shaA `
    -BName $BName -BCmd $BCmd -SourceB $sourceB -ShaB $shaB
$load = (1..5 | ForEach-Object { (Get-CimInstance Win32_Processor).LoadPercentage; Start-Sleep -Milliseconds 200 } |
           Where-Object { $_ -ne $null } | Measure-Object -Maximum).Maximum

@"
$provenanceMetadata
TC:            tc=$TC$(if ($ANodes -or $BNodes) { "  (nodes A=$ANodes B=$BNodes)" })
Seed:          $Seed
Book:          $Book  -format epd -order random -repeat -games 2
Affinity:      $(if ($useAffinity) { 'enabled' } else { 'disabled' })   Concurrency: $Concurrency   Threads: A=$AThreads B=$BThreads   Hash: $Hash
SPRT:          $(if ($Sprt) { $Sprt } else { '(none -- fixed-length match)' })
Rounds:        $Rounds (x2 games, paired openings)
Guardrail:     $affinityReason
Pre-run CPU:   ${load}% (max of 5 samples, AGENTS.md 4.8)
Purpose:       $Purpose
Date:          $((Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ'))
Driver:        harness/run_match.ps1
Command:
$cmdLine
"@ | Out-File -FilePath $metaPath -Encoding utf8

Write-Host "[metadata] $metaPath" -ForegroundColor Cyan
Write-Host "[command ] $cmdLine" -ForegroundColor DarkGray

# ---------------------------------------------------------------------------
# Run.
# ---------------------------------------------------------------------------
& $fc @args *>&1 | Tee-Object -FilePath $console
$fcExit = $LASTEXITCODE
"fastchess exit code: $fcExit" | Out-File -Append -FilePath $console

# ---------------------------------------------------------------------------
# RULE 2 -- per-player forfeit / crash accounting. PGN is authoritative.
# ---------------------------------------------------------------------------
$forfeits    = @{ $AName = 0; $BName = 0 }
$illegalPlay = @{ $AName = 0; $BName = 0 }
$adjudic     = 0
if (Test-Path $pgn) {
    $white = $null; $black = $null; $result = $null
    foreach ($line in [System.IO.File]::ReadLines($pgn)) {
        if     ($line -match '^\[White "(.+)"\]')  { $white  = $Matches[1] }
        elseif ($line -match '^\[Black "(.+)"\]')  { $black  = $Matches[1] }
        elseif ($line -match '^\[Result "(.+)"\]') { $result = $Matches[1] }
        elseif ($line -match '^\[Termination "time forfeit"\]') {
            # The forfeiting side is the loser of the game.
            $loser = switch ($result) { '1-0' { $black } '0-1' { $white } default { $null } }
            if ($loser -and $forfeits.ContainsKey($loser)) { $forfeits[$loser]++ }
            else { Write-Host "[warn] unattributable time forfeit (result=$result W=$white B=$black)" -ForegroundColor Yellow }
        }
        elseif ($line -match '^\[Termination "adjudication"\]') { $adjudic++ }
        elseif ($line -match 'makes an illegal move|illegal move') {
            # fastchess annotates the result line, e.g. "1-0 {Black makes an illegal move: ...}".
            $offender = $null
            if     ($line -match 'White makes an illegal move') { $offender = $white }
            elseif ($line -match 'Black makes an illegal move') { $offender = $black }
            if ($offender -and $illegalPlay.ContainsKey($offender)) { $illegalPlay[$offender]++ }
            else { Write-Host "[warn] unattributable illegal move in PGN: $line" -ForegroundColor Yellow }
        }
    }
}

# Console cross-check: fastchess prints "Player: <name>" / "Timeouts: N" / "Crashed: N".
$crashes  = @{ $AName = 0; $BName = 0 }
$conTimeo = @{ $AName = 0; $BName = 0 }
$current  = $null
foreach ($line in [System.IO.File]::ReadLines($console)) {
    if     ($line -match '^\s*Player:\s+(.+?)\s*$')   { $current = $Matches[1] }
    elseif ($line -match '^\s*Timeouts:\s+(\d+)')     { if ($current -and $conTimeo.ContainsKey($current)) { $conTimeo[$current] = [int]$Matches[1] } }
    elseif ($line -match '^\s*Crashed:\s+(\d+)')      { if ($current -and $crashes.ContainsKey($current))  { $crashes[$current]  = [int]$Matches[1] } }
}
# "Illegal PV move" is fastchess complaining about a PV *report*, not a played move; it is a
# reporting quirk and must be counted separately from an actual illegal move on the board,
# which is a fatal engine defect. Attribute both to the named engine.
$illegalPv = @{ $AName = 0; $BName = 0 }
$noOutput  = @{ $AName = 0; $BName = 0 }
foreach ($line in [System.IO.File]::ReadLines($console)) {
    if ($line -match 'Illegal PV move .* from (\S+)')  { $n = $Matches[1]; if ($illegalPv.ContainsKey($n)) { $illegalPv[$n]++ } }
    elseif ($line -match 'No output from (\S+)')       { $n = $Matches[1].TrimEnd(','); if ($noOutput.ContainsKey($n)) { $noOutput[$n]++ } }
}

$report = @()
$report += ''
$report += '================ HARNESS SELF-CHECK (AGENTS.md 4.451) ================'
$report += "affinity: $(if ($useAffinity) { 'enabled' } else { 'disabled' })   concurrency: $Concurrency   threads: A=$AThreads B=$BThreads"
foreach ($n in @($AName, $BName)) {
    $report += ("  {0,-12} time forfeits (PGN): {1,-4} console Timeouts: {2,-4} Crashed: {3,-4} illegal MOVES played: {4,-4} illegal PV reports: {5,-4} 'No output from': {6}" `
                -f $n, $forfeits[$n], $conTimeo[$n], $crashes[$n], $illegalPlay[$n], $illegalPv[$n], $noOutput[$n])
}
$report += "  adjudications: $adjudic"
$report += "  NOTE: 'illegal PV reports' are fastchess complaining about a PV line, NOT an illegal move on the board. Recorded, never fatal."
$report += '======================================================================'
$report | ForEach-Object { Write-Host $_ }
$report | Out-File -Append -FilePath $console
$report | Out-File -Append -FilePath $metaPath

$bad = @()
foreach ($n in @($AName, $BName)) {
    $t = [Math]::Max($forfeits[$n], $conTimeo[$n])
    $failures = @()
    if ($t -gt 0)              { $failures += "$t time forfeit(s)" }
    if ($crashes[$n] -gt 0)    { $failures += "$($crashes[$n]) crash(es)" }
    if ($illegalPlay[$n] -gt 0){ $failures += "$($illegalPlay[$n]) illegal move(s) PLAYED" }
    if ($failures.Count -eq 0) { continue }
    if ($ForfeitsAllowedFor -notcontains $n) {
        $bad += "$n : " + ($failures -join ', ')
    } else {
        Write-Host "[RECORDED, NOT FATAL] $n had $($failures -join ', '); -ForfeitsAllowedFor covers this engine. These are reported separately and are NOT charged to the other engine." -ForegroundColor Yellow
    }
}

if ($bad.Count -gt 0) {
    Fail ("Match result is NOT admissible evidence -- " + ($bad -join '; ') +
          ". Per AGENTS.md 4.451 a non-zero per-player forfeit count is the single signal that separates " +
          'an invalid harness configuration from a genuine engine defect. Investigate before quoting any Elo from this run.') 3
}

Write-Host "[ok] zero forfeits, zero crashes, zero illegal moves for every engine not on the allow-list." -ForegroundColor Green
exit $fcExit
