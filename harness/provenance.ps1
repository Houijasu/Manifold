function Get-ManifoldRepositoryRoot {
    (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

function Get-ManifoldHeadCommit([string]$GitCommand = 'git') {
    $root = Get-ManifoldRepositoryRoot
    $output = @(& $GitCommand -C $root rev-parse HEAD 2>&1)
    $exitCode = $LASTEXITCODE

    if ($exitCode -ne 0) {
        throw "Unable to determine Manifold HEAD: git exited with code $exitCode."
    }
    if ($output.Count -ne 1 -or [string]$output[0] -notmatch '\A[0-9a-fA-F]{40}\z') {
        throw 'Unable to determine Manifold HEAD: git did not return exactly one 40-hex commit.'
    }

    [string]$output[0]
}

function Get-BinarySourceAttestation([string]$BinaryPath) {
    $root = Get-ManifoldRepositoryRoot
    $binary = [System.IO.Path]::GetFullPath($BinaryPath)
    $target = [System.IO.Path]::GetFullPath((Join-Path $root 'target')) + [System.IO.Path]::DirectorySeparatorChar
    $sidecar = "$binary.source-commit"

    if (Test-Path -LiteralPath $sidecar) {
        $commit = [System.IO.File]::ReadAllText($sidecar)
        if ($commit -match '\A[0-9a-fA-F]{40}\z') {
            return [pscustomobject]@{ Commit = $commit; Mode = 'sidecar' }
        }

        return [pscustomobject]@{ Commit = 'unknown'; Mode = 'unattested' }
    }

    if ($binary.StartsWith($target, [System.StringComparison]::OrdinalIgnoreCase)) {
        return [pscustomobject]@{
            Commit = Get-ManifoldHeadCommit
            Mode = 'inferred-target-worktree'
        }
    }

    [pscustomobject]@{ Commit = 'unknown'; Mode = 'unattested' }
}

function Format-ManifoldMatchProvenanceMetadata {
    param(
        [string]$DriverCommit,
        [string]$AName,
        [string]$ACmd,
        [psobject]$SourceA,
        [string]$ShaA,
        [string]$BName,
        [string]$BCmd,
        [psobject]$SourceB,
        [string]$ShaB
    )

@"
Driver commit: $DriverCommit
Binary A:      $AName -> $ACmd
Source A:      $($SourceA.Commit)
Source mode A: $($SourceA.Mode)
SHA-256 A:     $ShaA
Binary B:      $BName -> $BCmd
Source B:      $($SourceB.Commit)
Source mode B: $($SourceB.Mode)
SHA-256 B:     $ShaB
"@
}
