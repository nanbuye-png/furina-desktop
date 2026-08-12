[CmdletBinding()]
param(
    [string]$ExecutablePath = "target/release/furina-desktop.exe",
    [string]$OutputPath = "dist/Furina-Desktop-0.1.2-x64-Portable.zip"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot ".."))
$exe = Join-Path $root $ExecutablePath
if (-not (Test-Path $exe)) {
    $candidate = Get-ChildItem (Join-Path $root "target/release") -Filter "*.exe" -File | Where-Object { $_.Name -match "furina" } | Select-Object -First 1
    if ($candidate) { $exe = $candidate.FullName }
}
if (-not (Test-Path $exe)) { throw "找不到 Tauri 可执行文件: $ExecutablePath" }

$sidecar = Join-Path $root "desktop/src-tauri/bin/furina-sidecar.exe"
if (-not (Test-Path $sidecar)) { throw "找不到 sidecar，请先构建 furina-sidecar.exe" }

$tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$stage = Join-Path $tempBase ("furina-portable-" + [guid]::NewGuid().ToString("N"))
if (-not ([System.IO.Path]::GetFullPath($stage).StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase))) { throw "临时目录越界" }
$zip = Join-Path $root $OutputPath
try {
    New-Item -ItemType Directory -Force -Path (Join-Path $stage "bin"), (Join-Path $stage "resources"), (Join-Path $stage "data") | Out-Null
    Copy-Item $exe (Join-Path $stage "furina-desktop.exe")
    Copy-Item $sidecar (Join-Path $stage "bin/furina-sidecar.exe")
    Copy-Item (Join-Path $root "persona") (Join-Path $stage "resources/persona") -Recurse
    New-Item -ItemType Directory -Force -Path (Join-Path $stage "resources/defaults") | Out-Null
    Copy-Item (Join-Path $root "desktop/resources/defaults/config.yaml") (Join-Path $stage "resources/defaults/config.yaml")
    New-Item -ItemType File -Force -Path (Join-Path $stage "portable.flag") | Out-Null
    New-Item -ItemType Directory -Force -Path (Split-Path (Join-Path $root $OutputPath) -Parent) | Out-Null
    if (Test-Path $zip) { Remove-Item $zip -Force }
    Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip -CompressionLevel Optimal
} finally {
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
}
Write-Output ("portable: " + (Resolve-Path $zip))
