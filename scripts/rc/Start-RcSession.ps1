[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet("Portable", "Installed")]
    [string]$Mode,
    [string]$KitRoot = "C:\FurinaRcKit",
    [string]$ResultRoot = "C:\FurinaRcResults"
)

$ErrorActionPreference = "Stop"
$sessionRoot = Join-Path "C:\FurinaRc" $Mode
New-Item -ItemType Directory -Force -Path $sessionRoot, $ResultRoot, "C:\project" | Out-Null
Copy-Item (Join-Path $KitRoot "fixtures/project/*") "C:\project" -Recurse -Force
Copy-Item (Join-Path $KitRoot "fixtures/workspace") (Join-Path $sessionRoot "workspace") -Recurse -Force

function Get-CommandStatus([string]$Name) {
    $command = Get-Command $Name -ErrorAction SilentlyContinue
    [ordered]@{
        found = [bool]$command
        source = if ($command) { $command.Source } else { "" }
        commandType = if ($command) { $command.CommandType.ToString() } else { "" }
    }
}

$commands = [ordered]@{
    python = Get-CommandStatus "python"
    node = Get-CommandStatus "node"
    rustc = Get-CommandStatus "rustc"
    cargo = Get-CommandStatus "cargo"
    git = Get-CommandStatus "git"
}
$computer = Get-CimInstance Win32_ComputerSystem
$os = Get-CimInstance Win32_OperatingSystem
$processors = @(Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors)
$video = @(Get-CimInstance Win32_VideoController | Select-Object Name, DriverVersion)
$network = @(Get-NetConnectionProfile -ErrorAction SilentlyContinue | Select-Object Name, InterfaceAlias, NetworkCategory, IPv4Connectivity, IPv6Connectivity)
$environment = [ordered]@{
    timestamp = (Get-Date).ToString("o")
    mode = $Mode
    os = $os.Caption
    osVersion = $os.Version
    buildNumber = $os.BuildNumber
    totalMemoryGiB = [math]::Round($computer.TotalPhysicalMemory / 1GB, 2)
    processors = $processors
    video = $video
    network = $network
    commands = $commands
}
$environment | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $ResultRoot "environment.json") -Encoding utf8
$checklistTarget = Join-Path $ResultRoot "rc-checklist.csv"
if (-not (Test-Path $checklistTarget)) {
    $template = Import-Csv -LiteralPath (Join-Path $KitRoot "rc-checklist.csv")
    $template | Where-Object { $_.Mode -in @("Both", $Mode) -or ($_.Mode -eq "Selected" -and $Mode -eq "Portable") } | Export-Csv -LiteralPath $checklistTarget -NoTypeInformation -Encoding utf8
}
$issuesTarget = Join-Path $ResultRoot "rc-issues.csv"
if (-not (Test-Path $issuesTarget)) { Copy-Item (Join-Path $KitRoot "rc-issues.csv") $issuesTarget }
$guideTarget = Join-Path $ResultRoot "RC_ACCEPTANCE_TEST_0.1.2.md"
if (-not (Test-Path $guideTarget)) { Copy-Item (Join-Path $KitRoot "RC_ACCEPTANCE_TEST_0.1.2.md") $guideTarget }

if ($Mode -eq "Portable") {
    $archive = Join-Path $KitRoot "artifacts/Furina-Desktop-0.1.2-x64-Portable.zip"
    $appRoot = Join-Path $sessionRoot "app"
    Expand-Archive -LiteralPath $archive -DestinationPath $appRoot -Force
    if (-not (Test-Path (Join-Path $appRoot "portable.flag"))) { throw "portable.flag 缺失" }
    $env:FURINA_WORKSPACE = Join-Path $sessionRoot "workspace"
    Start-Process -FilePath (Join-Path $appRoot "furina-desktop.exe") -WorkingDirectory $appRoot
    Write-Output "portable_app=$appRoot"
    Write-Output "expected_data=$(Join-Path $appRoot 'data/.furina')"
} else {
    $setupTarget = Join-Path $sessionRoot "Furina-Desktop-0.1.2-x64-Setup.exe"
    Copy-Item -LiteralPath (Join-Path $KitRoot "artifacts/Furina-Desktop-0.1.2-x64-Setup.exe") $setupTarget -Force
    Start-Process explorer.exe -ArgumentList "/select,$setupTarget"
    Write-Output "setup=$setupTarget"
    Write-Output "expected_data=$env:APPDATA\com.furina.lifeform\.furina"
}

Start-Process notepad.exe -ArgumentList (Join-Path $ResultRoot "RC_ACCEPTANCE_TEST_0.1.2.md")
Write-Output "results=$ResultRoot"
Write-Output "legacy_desktop=C:\project\legacy-desktop"
Write-Output "legacy_cli=C:\project\legacy-cli"
$soakHelper = Join-Path $sessionRoot "Start-Soak.ps1"
@"
& C:\FurinaRcKit\scripts\Start-RcSoak.ps1 -OutputDirectory "$ResultRoot"
"@ | Set-Content -LiteralPath $soakHelper -Encoding utf8
Write-Output "soak_command=powershell -ExecutionPolicy Bypass -File $soakHelper"
Write-Output "workspace=$(Join-Path $sessionRoot 'workspace')"
