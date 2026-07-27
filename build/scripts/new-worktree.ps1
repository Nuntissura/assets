[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern("^[A-Za-z0-9][A-Za-z0-9._-]*$")]
    [string]$WorktreeId,

    [Parameter(Mandatory = $true)]
    [ValidatePattern("^codex/[A-Za-z0-9][A-Za-z0-9._/-]*$")]
    [string]$Branch,

    [string]$StartPoint = "main"
)

$ErrorActionPreference = "Stop"
$invocationRoot = (& git -C (Join-Path $PSScriptRoot "../..") rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($invocationRoot)) {
    throw "FF-WORKTREE-E-ROOT: repository root resolution failed"
}
$commonGitDir = (& git -C $invocationRoot rev-parse --path-format=absolute --git-common-dir).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($commonGitDir)) {
    throw "FF-WORKTREE-E-ROOT: Git common-directory resolution failed"
}
$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $commonGitDir))
$worktreeRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot ".worktrees"))
$destination = [System.IO.Path]::GetFullPath((Join-Path $worktreeRoot $WorktreeId))
if ([System.IO.Directory]::GetParent($destination).FullName -ne $worktreeRoot) {
    throw "FF-WORKTREE-E-BOUNDARY: destination is outside .worktrees"
}
if (Test-Path -LiteralPath $destination) {
    throw "FF-WORKTREE-E-EXISTS: destination already exists"
}
[System.IO.Directory]::CreateDirectory($worktreeRoot) | Out-Null
& git -C $repoRoot worktree add --lock --reason $WorktreeId -b $Branch $destination $StartPoint
if ($LASTEXITCODE -ne 0) {
    throw "FF-WORKTREE-E-ADD: git worktree add failed"
}
$artifactRoot = [System.IO.Path]::GetFullPath((Join-Path $repoRoot ".fforager-artifacts"))
$worktreeArtifactLink = [System.IO.Path]::GetFullPath(
    (Join-Path $destination ".fforager-artifacts")
)
try {
    [System.IO.Directory]::CreateDirectory($artifactRoot) | Out-Null
    if ([System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT) {
        New-Item -ItemType Junction -Path $worktreeArtifactLink -Target $artifactRoot -ErrorAction Stop | Out-Null
    } else {
        New-Item -ItemType SymbolicLink -Path $worktreeArtifactLink -Target $artifactRoot -ErrorAction Stop | Out-Null
    }
} catch {
    & git -C $repoRoot worktree unlock $destination 2>$null
    & git -C $repoRoot worktree remove --force $destination 2>$null
    & git -C $repoRoot branch -D $Branch 2>$null
    throw "FF-WORKTREE-E-ARTIFACT-LINK: failed to link the shared artifact root: $($_.Exception.Message)"
}
Write-Output "PASS FF-WORKTREE-ROOT-001; worktree=$WorktreeId; artifact_root=.fforager-artifacts"
