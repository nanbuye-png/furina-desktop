[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot ".."))
$toolsRoot = Join-Path $root "target/.tauri"
$nsisRoot = Join-Path $toolsRoot "NSIS"
$required = @(
    "makensis.exe",
    "Bin/makensis.exe",
    "Include/MUI2.nsh",
    "Plugins/x86-unicode/additional/nsis_tauri_utils.dll"
)
$pluginPath = Join-Path $nsisRoot "Plugins/x86-unicode/additional/nsis_tauri_utils.dll"
if (($required | ForEach-Object { Test-Path (Join-Path $nsisRoot $_) }) -notcontains $false) {
    if ((Get-FileHash $pluginPath -Algorithm SHA1).Hash -eq "75197FEE3C6A814FE035788D1C34EAD39349B860") {
        Write-Output ("NSIS cache ready: " + $nsisRoot)
        exit 0
    }
}

$tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$temp = Join-Path $tempBase ("furina-nsis-" + [guid]::NewGuid().ToString("N"))
if (-not ([System.IO.Path]::GetFullPath($temp).StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase))) {
    throw "临时目录越界"
}
$targetFull = [System.IO.Path]::GetFullPath($nsisRoot)
$expectedTargetRoot = [System.IO.Path]::GetFullPath((Join-Path $root "target"))
if (-not $targetFull.StartsWith($expectedTargetRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "NSIS 目标目录越界"
}

New-Item -ItemType Directory -Force -Path $temp | Out-Null
try {
    $archive = Join-Path $temp "nsis-3.11.zip"
    $plugin = Join-Path $temp "nsis_tauri_utils.dll"
    Invoke-WebRequest "https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip" -OutFile $archive -TimeoutSec 600
    if ((Get-FileHash $archive -Algorithm SHA1).Hash -ne "EF7FF767E5CBD9EDD22ADD3A32C9B8F4500BB10D") { throw "NSIS 压缩包 SHA-1 校验失败" }
    Invoke-WebRequest "https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll" -OutFile $plugin -TimeoutSec 600
    if ((Get-FileHash $plugin -Algorithm SHA1).Hash -ne "75197FEE3C6A814FE035788D1C34EAD39349B860") { throw "nsis_tauri_utils.dll SHA-1 校验失败" }
    Expand-Archive $archive (Join-Path $temp "extract") -Force
    $source = Join-Path $temp "extract/nsis-3.11"
    New-Item -ItemType Directory -Force -Path (Join-Path $source "Plugins/x86-unicode/additional") | Out-Null
    Copy-Item $plugin (Join-Path $source "Plugins/x86-unicode/additional/nsis_tauri_utils.dll") -Force
    New-Item -ItemType Directory -Force -Path $toolsRoot | Out-Null
    if (Test-Path $nsisRoot) { Remove-Item $nsisRoot -Recurse -Force }
    Move-Item $source $nsisRoot
} finally {
    if (Test-Path $temp) { Remove-Item $temp -Recurse -Force }
}
Write-Output ("NSIS cache ready: " + $nsisRoot)
