function Get-ManifoldRepositoryRoot {
    (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
}

function Get-ManifoldHeadCommit {
    param(
        [string]$RepositoryRoot = '',
        [string]$GitCommand = 'git'
    )

    if ([string]::IsNullOrWhiteSpace($RepositoryRoot)) {
        $RepositoryRoot = Get-ManifoldRepositoryRoot
    }
    $output = @(& $GitCommand -C $RepositoryRoot rev-parse HEAD 2>&1)
    $exitCode = $LASTEXITCODE

    if ($exitCode -ne 0) {
        throw "Unable to determine HEAD for '$RepositoryRoot': git exited with code $exitCode."
    }
    if ($output.Count -ne 1 -or [string]$output[0] -notmatch '\A[0-9a-fA-F]{40}\z') {
        throw "Unable to determine HEAD for '$RepositoryRoot': git did not return exactly one 40-hex commit."
    }

    [string]$output[0]
}

function Get-ContainingGitWorktreeRoot([string]$Path, [string]$GitCommand = 'git') {
    $directory = [System.IO.DirectoryInfo]::new([System.IO.Path]::GetDirectoryName([System.IO.Path]::GetFullPath($Path)))

    while ($null -ne $directory) {
        if (Test-Path -LiteralPath (Join-Path $directory.FullName '.git')) {
            $output = @(& $GitCommand -C $directory.FullName rev-parse --show-toplevel 2>&1)
            $exitCode = $LASTEXITCODE
            if ($exitCode -ne 0) {
                throw "Unable to validate Git worktree '$($directory.FullName)': git exited with code $exitCode."
            }
            if ($output.Count -ne 1) {
                throw "Unable to validate Git worktree '$($directory.FullName)': git did not return exactly one root."
            }

            $reportedRoot = [System.IO.Path]::GetFullPath([string]$output[0])
            if (-not $reportedRoot.Equals($directory.FullName, [System.StringComparison]::OrdinalIgnoreCase)) {
                return $null
            }

            return $directory.FullName
        }

        $directory = $directory.Parent
    }

    $null
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

    $containingWorktree = Get-ContainingGitWorktreeRoot $binary
    if ($null -ne $containingWorktree) {
        return [pscustomobject]@{
            Commit = Get-ManifoldHeadCommit -RepositoryRoot $containingWorktree
            Mode = 'inferred-containing-worktree'
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
