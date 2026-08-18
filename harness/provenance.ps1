function Get-ManifoldRepositoryRoot {
    (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
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
            Commit = (& git -C $root rev-parse HEAD).Trim()
            Mode = 'inferred-target-worktree'
        }
    }

    [pscustomobject]@{ Commit = 'unknown'; Mode = 'unattested' }
}
