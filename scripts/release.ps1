<#
.SYNOPSIS
    Bumps StreamHub version, builds, commits, tags, and publishes a release.
.DESCRIPTION
    Starts from v1.0. When -Version is omitted, automatically increments x.(x+1)
    (e.g. 1.0 -> 1.1 -> 1.2). When -Version is specified (e.g. 1.0, 2.0, 1.2.1),
    uses that exact version.
.EXAMPLE
    .\scripts\release.ps1
    .\scripts\release.ps1 -Version 1.0
    .\scripts\release.ps1 -Version 2.0 -NoPush
#>
param(
    [string]$Version = "",
    [switch]$NoPush,
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$cargoToml = Join-Path $repoRoot "Cargo.toml"

if (-not (Test-Path $cargoToml)) {
    throw "Cargo.toml not found at $cargoToml"
}

$content = Get-Content -Raw -Path $cargoToml
if ($content -notmatch '(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"') {
    throw "Could not parse current version from Cargo.toml"
}

$curMajor = [int]$Matches[1]
$curMinor = [int]$Matches[2]

if ([string]::IsNullOrWhiteSpace($Version)) {
    # Default bump: x.(x+1)
    $newMajor = $curMajor
    $newMinor = $curMinor + 1
    $newPatch = 0
} else {
    $clean = $Version.Trim().TrimStart('v', 'V')
    $parts = $clean.Split('.')
    if ($parts.Count -lt 2 -or $parts.Count -gt 3) {
        throw "Invalid version '$Version'. Expected format: X.Y or X.Y.Z (e.g. 1.0, 1.1, 2.0.1)"
    }
    $newMajor = [int]$parts[0]
    $newMinor = [int]$parts[1]
    $newPatch = if ($parts.Count -eq 3) { [int]$parts[2] } else { 0 }
}

$cargoVersion = "$newMajor.$newMinor.$newPatch"
$tagVersion = if ($newPatch -eq 0) { "v$newMajor.$newMinor" } else { "v$newMajor.$newMinor.$newPatch" }

Write-Host "Current Cargo.toml version : $curMajor.$curMinor.$($Matches[3])"
Write-Host "Target Cargo.toml version  : $cargoVersion"
Write-Host "Target GitHub release tag  : $tagVersion"

$updated = [regex]::Replace(
    $content,
    '(?m)^version\s*=\s*"\d+\.\d+\.\d+"',
    "version = `"$cargoVersion`"",
    1
)
Set-Content -Path $cargoToml -Value $updated -NoNewline -Encoding UTF8

Push-Location $repoRoot
try {
    if (-not $SkipBuild) {
        Write-Host "Running cargo test..."
        cargo test
        if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

        Write-Host "Building release binary..."
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }

        $distDir = Join-Path $repoRoot "dist"
        $stageDir = Join-Path $distDir "package"
        if (Test-Path $stageDir) { Remove-Item -Recurse -Force $stageDir }
        New-Item -ItemType Directory -Force -Path $stageDir | Out-Null
        Copy-Item "target\release\streamhub.exe" "$stageDir\streamhub.exe"
        Copy-Item "libmpv\libmpv-2.dll" "$stageDir\libmpv-2.dll"
        $zipPath = Join-Path $distDir "StreamHub-$tagVersion-windows-x64.zip"
        if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
        Compress-Archive -Path "$stageDir\*" -DestinationPath $zipPath -Force
        Write-Host "Packaged: $zipPath"
    }

    $gitExe = (Get-Command git -ErrorAction SilentlyContinue).Source
    if (-not $gitExe -and (Test-Path "C:\Program Files\Git\cmd\git.exe")) {
        $gitExe = "C:\Program Files\Git\cmd\git.exe"
    }
    if (-not $gitExe) {
        Write-Warning "git not found in PATH; skipping commit/tag/push."
        return
    }

    & $gitExe add Cargo.toml Cargo.lock
    $hasStaged = (& $gitExe diff --cached --name-only)
    if ($hasStaged) {
        & $gitExe commit -m "Release $tagVersion"
    }
    & $gitExe tag -a $tagVersion -m "Release $tagVersion"

    if (-not $NoPush) {
        & $gitExe push origin HEAD
        & $gitExe push origin $tagVersion
        Write-Host "Pushed $tagVersion to origin. GitHub Actions will publish the release."
    } else {
        Write-Host "Prepared tag $tagVersion locally (-NoPush)."
    }
} finally {
    Pop-Location
}
