[CmdletBinding()]
param(
    [string]$OutputPath = "desktop/src-tauri/bin/furina-sidecar.exe"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot ".."))
$pythonRoot = Join-Path $root "python"
$entry = Join-Path $pythonRoot "furina_tools/sidecar_entry.py"
$output = Join-Path $root $OutputPath
$outputDir = Split-Path $output -Parent

& python -c "import PyInstaller"
if ($LASTEXITCODE -ne 0) { throw "PyInstaller 未安装。请先运行: python -m pip install pyinstaller" }

New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
$tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$temp = Join-Path $tempBase ("furina-sidecar-" + [guid]::NewGuid().ToString("N"))
if (-not ([System.IO.Path]::GetFullPath($temp).StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase))) { throw "临时目录越界" }
New-Item -ItemType Directory -Force -Path $temp | Out-Null
try {
    & python -m PyInstaller --noconfirm --clean --onefile --name furina-sidecar --paths $pythonRoot --distpath $temp/dist --workpath $temp/build --specpath $temp $entry
    if ($LASTEXITCODE -ne 0) { throw "PyInstaller 构建失败，退出码 $LASTEXITCODE" }
    Copy-Item (Join-Path $temp "dist/furina-sidecar.exe") $output -Force
} finally {
    Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output ("sidecar: " + (Resolve-Path $output))
