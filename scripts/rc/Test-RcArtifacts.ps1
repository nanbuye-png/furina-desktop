[CmdletBinding()]
param(
    [string]$ArtifactDirectory = "dist",
    [string]$OutputPath = ""
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "../.."))
$artifactRoot = if ([System.IO.Path]::IsPathRooted($ArtifactDirectory)) { [System.IO.Path]::GetFullPath($ArtifactDirectory) } else { [System.IO.Path]::GetFullPath((Join-Path $root $ArtifactDirectory)) }
$setup = Join-Path $artifactRoot "Furina-Desktop-0.1.2-x64-Setup.exe"
$portable = Join-Path $artifactRoot "Furina-Desktop-0.1.2-x64-Portable.zip"
$sums = Join-Path $artifactRoot "SHA256SUMS.txt"
foreach ($candidate in @($setup, $portable, $sums)) { if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) { throw "缺少 RC1 产物: $candidate" } }
$expected = @{}
foreach ($line in Get-Content -LiteralPath $sums) { if ($line -match '^([0-9a-fA-F]{64})\s+(.+)$') { $expected[$Matches[2].Trim()] = $Matches[1].ToLowerInvariant() } }
$hashResults = foreach ($candidate in @($setup, $portable)) {
    $name = Split-Path $candidate -Leaf
    $actual = (Get-FileHash -LiteralPath $candidate -Algorithm SHA256).Hash.ToLowerInvariant()
    [pscustomobject]@{ file = $name; expected = $expected[$name]; actual = $actual; passed = $expected.ContainsKey($name) -and $expected[$name] -eq $actual }
}
$entries = @(tar -tf $portable)
if ($LASTEXITCODE -ne 0) { throw "无法读取便携 ZIP 内容" }
$requiredEntries = @("furina-desktop.exe", "portable.flag", "bin/furina-sidecar.exe", "resources/defaults/config.yaml", "resources/persona/furina.yaml")
$missingEntries = @($requiredEntries | Where-Object { $_ -notin $entries })
$forbiddenEntries = @($entries | Where-Object { $_ -match '(^|/)(secrets\.env$|Furina\.vrm$|instance\.lock$)' -or $_ -match '(^|/)(memory|voice|web_cache)/' })
$trackedForbidden = @()
if (Test-Path (Join-Path $root ".git")) {
    $safeRoot = $root.Path.Replace('\', '/')
    $tracked = @(git -c "safe.directory=$safeRoot" -C $root ls-files)
    if ($LASTEXITCODE -ne 0) { throw "无法读取 Git 跟踪文件" }
    $trackedForbidden = @($tracked | Where-Object { $_ -match '(^|/)(secrets\.env$|Furina\.vrm$|instance\.lock$)' -or $_ -match '(^|/)(memory|voice|web_cache)/' })
}
$report = [ordered]@{
    generatedAt = (Get-Date).ToString("o")
    artifactDirectory = $artifactRoot
    hashes = $hashResults
    portableEntries = $entries
    missingEntries = $missingEntries
    forbiddenEntries = $forbiddenEntries
    trackedForbidden = $trackedForbidden
    passed = @($hashResults | Where-Object { -not $_.passed }).Count -eq 0 -and $missingEntries.Count -eq 0 -and $forbiddenEntries.Count -eq 0 -and $trackedForbidden.Count -eq 0
}
if (-not $OutputPath) { $OutputPath = Join-Path $artifactRoot "rc1-artifact-check.json" }
$outputFull = if ([System.IO.Path]::IsPathRooted($OutputPath)) { [System.IO.Path]::GetFullPath($OutputPath) } else { [System.IO.Path]::GetFullPath((Join-Path $root $OutputPath)) }
New-Item -ItemType Directory -Force -Path (Split-Path $outputFull -Parent) | Out-Null
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $outputFull -Encoding utf8
$hashResults | Format-Table -AutoSize
Write-Output "artifact_report=$outputFull"
Write-Output "artifact_check_passed=$($report.passed)"
if (-not $report.passed) { exit 1 }
