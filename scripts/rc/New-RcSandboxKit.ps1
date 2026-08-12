[CmdletBinding()]
param(
    [string]$OutputDirectory = "dist/rc1-sandbox-kit",
    [string]$ResultDirectory = "dist/rc1-results"
)

$ErrorActionPreference = "Stop"
$root = (Resolve-Path (Join-Path $PSScriptRoot "../.."))
$output = [System.IO.Path]::GetFullPath((Join-Path $root $OutputDirectory))
$results = [System.IO.Path]::GetFullPath((Join-Path $root $ResultDirectory))
$distRoot = [System.IO.Path]::GetFullPath((Join-Path $root "dist"))
foreach ($candidate in @($output, $results)) { if (-not $candidate.StartsWith($distRoot, [System.StringComparison]::OrdinalIgnoreCase)) { throw "RC 工具包和结果目录必须位于项目 dist 目录" } }
if (Test-Path $output) { Remove-Item -LiteralPath $output -Recurse -Force }
New-Item -ItemType Directory -Force -Path @((Join-Path $output "artifacts"), (Join-Path $output "scripts"), (Join-Path $output "fixtures"), (Join-Path $results "portable"), (Join-Path $results "installed")) | Out-Null
& (Join-Path $PSScriptRoot "Test-RcArtifacts.ps1") -OutputPath (Join-Path $output "artifact-check.json")
if ($LASTEXITCODE -ne 0) { throw "RC 产物预检失败" }
& (Join-Path $PSScriptRoot "New-RcFixtures.ps1") -OutputDirectory (Join-Path $output "fixtures")
Copy-Item (Join-Path $root "dist/Furina-Desktop-0.1.2-x64-Setup.exe") (Join-Path $output "artifacts")
Copy-Item (Join-Path $root "dist/Furina-Desktop-0.1.2-x64-Portable.zip") (Join-Path $output "artifacts")
Copy-Item (Join-Path $root "dist/SHA256SUMS.txt") (Join-Path $output "artifacts")
Copy-Item (Join-Path $PSScriptRoot "Start-RcSession.ps1") (Join-Path $output "scripts")
Copy-Item (Join-Path $PSScriptRoot "Start-RcSoak.ps1") (Join-Path $output "scripts")
Copy-Item (Join-Path $PSScriptRoot "Test-RcGate.ps1") (Join-Path $output "scripts")
Copy-Item (Join-Path $root "tests/rc/rc-checklist.csv") (Join-Path $output "rc-checklist.csv")
Copy-Item (Join-Path $root "tests/rc/rc-issues.csv") (Join-Path $output "rc-issues.csv")
Copy-Item (Join-Path $root "docs/RC_ACCEPTANCE_TEST_0.1.2.md") (Join-Path $output "RC_ACCEPTANCE_TEST_0.1.2.md")
function Write-SandboxConfig([string]$Mode) {
    $command = "powershell.exe -ExecutionPolicy Bypass -File C:\FurinaRcKit\scripts\Start-RcSession.ps1 -Mode $Mode -KitRoot C:\FurinaRcKit -ResultRoot C:\FurinaRcResults\$Mode"
    $xml = @"
<Configuration>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$output</HostFolder>
      <SandboxFolder>C:\FurinaRcKit</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$results</HostFolder>
      <SandboxFolder>C:\FurinaRcResults</SandboxFolder>
      <ReadOnly>false</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <Networking>Enable</Networking>
  <AudioInput>Enable</AudioInput>
  <VideoInput>Disable</VideoInput>
  <ClipboardRedirection>Enable</ClipboardRedirection>
  <MemoryInMB>4096</MemoryInMB>
  <LogonCommand><Command>$command</Command></LogonCommand>
</Configuration>
"@
    $xml | Set-Content -LiteralPath (Join-Path $output "Furina-RC1-$Mode.wsb") -Encoding utf8
}
Write-SandboxConfig "Portable"
Write-SandboxConfig "Installed"
@"
Furina Desktop v0.1.2 RC1 Sandbox Kit

1. Run Furina-RC1-Portable.wsb and complete the portable checklist.
2. Close Sandbox to destroy all entered API keys.
3. Run Furina-RC1-Installed.wsb and complete install/uninstall/reinstall checks.
4. Run the mode-specific 2-hour soak command printed by Start-RcSession.ps1.
5. Fill the mode-specific files under C:\FurinaRcResults\Portable or C:\FurinaRcResults\Installed.
6. After both sessions, evaluate the combined gate:
   powershell -ExecutionPolicy Bypass -File C:\FurinaRcKit\scripts\Test-RcGate.ps1 -ResultDirectory C:\FurinaRcResults\Portable,C:\FurinaRcResults\Installed -RequireSoak

Never write real API keys, full secrets.env, or private VRM assets into the mapped result folder.
"@ | Set-Content -LiteralPath (Join-Path $output "README.txt") -Encoding utf8
Write-Output "sandbox_kit=$output"
Write-Output "results=$results"
Write-Output "portable_wsb=$(Join-Path $output 'Furina-RC1-Portable.wsb')"
Write-Output "installed_wsb=$(Join-Path $output 'Furina-RC1-Installed.wsb')"
