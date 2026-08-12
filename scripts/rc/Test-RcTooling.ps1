[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "../.."))
$tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$temp = Join-Path $tempBase ("furina-rc-tooling-" + [guid]::NewGuid().ToString("N"))
if (-not ([System.IO.Path]::GetFullPath($temp).StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase))) { throw "临时目录越界" }
New-Item -ItemType Directory -Force -Path $temp | Out-Null
try {
    $parseFailed = $false
    foreach ($script in Get-ChildItem $PSScriptRoot -Filter "*.ps1") {
        $tokens = $null
        $errors = $null
        [System.Management.Automation.Language.Parser]::ParseFile($script.FullName, [ref]$tokens, [ref]$errors) | Out-Null
        if ($errors.Count -gt 0) { $parseFailed = $true; $errors | ForEach-Object { Write-Error ("{0}: {1}" -f $script.Name, $_.Message) } }
    }
    if ($parseFailed) { throw "RC PowerShell 语法检查失败" }

    $fixtures = Join-Path $temp "fixtures"
    & (Join-Path $PSScriptRoot "New-RcFixtures.ps1") -OutputDirectory $fixtures | Out-Null
    foreach ($required in @(
        "project/legacy-desktop/.furina/config.yaml",
        "project/legacy-desktop/.furina/secrets.env",
        "project/legacy-desktop/.furina/avatar/Furina.vrm",
        "project/legacy-desktop/persona/furina.yaml",
        "project/legacy-desktop/python/furina_tools/server.py",
        "project/legacy-cli/.furina/config.yaml",
        "workspace/README.txt",
        "invalid-avatar.vrm",
        "wrong-extension.txt"
    )) {
        if (-not (Test-Path -LiteralPath (Join-Path $fixtures $required))) { throw "RC 夹具缺失: $required" }
    }
    if (Test-Path (Join-Path $fixtures "project/legacy-cli/persona/furina.yaml")) { throw "CLI 夹具不得包含 Desktop persona 标记" }
    $magic = [System.IO.File]::ReadAllBytes((Join-Path $fixtures "project/legacy-desktop/.furina/avatar/Furina.vrm"))[0..3]
    if ([System.Text.Encoding]::ASCII.GetString($magic) -ne "glTF") { throw "迁移 VRM 夹具头无效" }

    $portable = Join-Path $temp "Portable"
    $installed = Join-Path $temp "Installed"
    New-Item -ItemType Directory -Force -Path $portable, $installed | Out-Null
    $template = Import-Csv (Join-Path $root "tests/rc/rc-checklist.csv")
    foreach ($mode in @("Portable", "Installed")) {
        $target = if ($mode -eq "Portable") { $portable } else { $installed }
        $rows = @($template | Where-Object { $_.Mode -in @("Both", $mode) -or ($_.Mode -eq "Selected" -and $mode -eq "Portable") })
        $rows | ForEach-Object { $_.Status = "PASS" }
        $rows | Export-Csv (Join-Path $target "rc-checklist.csv") -NoTypeInformation -Encoding utf8
        "Id,Severity,Status,Area,Mode,Summary,Reproduction,Expected,Actual,Evidence,Disposition" | Set-Content (Join-Path $target "rc-issues.csv") -Encoding utf8
    }
    @{ passed = $true; elapsedMinutes = 120.0 } | ConvertTo-Json | Set-Content (Join-Path $portable "soak-summary.json") -Encoding utf8
    $powershell = (Get-Process -Id $PID).Path
    & $powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "Test-RcGate.ps1") -ResultDirectory "$portable,$installed" -RequireSoak | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "RC 通过门禁自测失败" }

    'RC-1,P1,OPEN,Sidecar,Portable,blocking test,repro,expected,actual,,fix before release' | Add-Content (Join-Path $portable "rc-issues.csv") -Encoding utf8
    & $powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "Test-RcGate.ps1") -ResultDirectory "$portable,$installed" -RequireSoak | Out-Null
    if ($LASTEXITCODE -eq 0) { throw "RC P1 阻断门禁自测失败" }
    Write-Output "rc_tooling_self_test_passed=True"
    $global:LASTEXITCODE = 0
} finally {
    if ((Test-Path $temp) -and [System.IO.Path]::GetFullPath($temp).StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase)) { Remove-Item -LiteralPath $temp -Recurse -Force }
}
