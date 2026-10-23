[CmdletBinding()]
param(
    [switch]$VerifyOnly
)

$ErrorActionPreference = "Stop"
$invocationRoot = (& git -C (Join-Path $PSScriptRoot "../..") rev-parse --show-toplevel).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($invocationRoot)) {
    throw "FF-ARTIFACT-E-ROOT: repository root resolution failed"
}
$commonGitDir = (& git -C $invocationRoot rev-parse --path-format=absolute --git-common-dir).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($commonGitDir)) {
    throw "FF-ARTIFACT-E-ROOT: Git common-directory resolution failed"
}
$repoRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $commonGitDir))
$artifactRoot = [System.IO.Path]::GetFullPath(
    (Join-Path $repoRoot ".fforager-artifacts")
)
if ([System.IO.Directory]::GetParent($artifactRoot).FullName -ne $repoRoot) {
    throw "FF-ARTIFACT-E-BOUNDARY: artifact root is not an immediate repository child"
}
[System.IO.Directory]::CreateDirectory($artifactRoot) | Out-Null
$children = @(Get-ChildItem -LiteralPath $artifactRoot -Force)
if ($VerifyOnly) {
    if ($children.Count -ne 0) {
        throw "FF-ARTIFACT-E-NOT-CLEAN: .fforager-artifacts contains $($children.Count) item(s)"
    }
    Write-Output "PASS FF-GATE-ARTIFACT-HYGIENE-001; items=0"
    exit 0
}
foreach ($child in $children) {
    Remove-Item -LiteralPath $child.FullName -Recurse -Force
}
if (@(Get-ChildItem -LiteralPath $artifactRoot -Force).Count -ne 0) {
    throw "FF-ARTIFACT-E-CLEANUP: artifact root is not empty after cleanup"
}
Write-Output "PASS FF-GATE-ARTIFACT-HYGIENE-001; items=0"
