[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $output | Out-Null

function Write-SyntheticVrm([string]$Path) {
    $document = '{"asset":{"version":"2.0","generator":"Furina RC migration fixture"},"extensionsUsed":["VRMC_vrm"],"extensions":{"VRMC_vrm":{"specVersion":"1.0"}}}'
    $json = [System.Collections.Generic.List[byte]]::new()
    $json.AddRange([System.Text.Encoding]::UTF8.GetBytes($document))
    while (($json.Count % 4) -ne 0) { $json.Add([byte][char]' ') }
    $total = 20 + $json.Count
    $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Create)
    $writer = $null
    try {
        $writer = [System.IO.BinaryWriter]::new($stream)
        $writer.Write([System.Text.Encoding]::ASCII.GetBytes("glTF"))
        $writer.Write([uint32]2)
        $writer.Write([uint32]$total)
        $writer.Write([uint32]$json.Count)
        $writer.Write([uint32]0x4E4F534A)
        $writer.Write($json.ToArray())
        $writer.Flush()
    } finally {
        if ($writer) { $writer.Dispose() } else { $stream.Dispose() }
    }
}

$legacy = Join-Path $output "project/legacy-desktop"
$cli = Join-Path $output "project/legacy-cli"
$workspace = Join-Path $output "workspace"
$directories = @(
    (Join-Path $legacy ".furina/memory"),
    (Join-Path $legacy ".furina/avatar"),
    (Join-Path $legacy ".furina/voice"),
    (Join-Path $legacy ".furina/web_cache"),
    (Join-Path $legacy "persona"),
    (Join-Path $legacy "python/furina_tools"),
    (Join-Path $legacy "target"),
    (Join-Path $cli ".furina/memory"),
    $workspace
)
New-Item -ItemType Directory -Force -Path $directories | Out-Null

@'
model: rc-migration-dummy
persona: furina
voice:
  enabled: false
asr:
  enabled: false
'@ | Set-Content -LiteralPath (Join-Path $legacy ".furina/config.yaml") -Encoding utf8
"FURINA_API_KEY=RC_DUMMY_NOT_A_REAL_SECRET" | Set-Content -LiteralPath (Join-Path $legacy ".furina/secrets.env") -Encoding ascii
'{"fixture":true}' | Set-Content -LiteralPath (Join-Path $legacy ".furina/memory/emotion.json") -Encoding utf8
"lock-must-not-migrate" | Set-Content -LiteralPath (Join-Path $legacy ".furina/memory/instance.lock") -Encoding ascii
"voice-cache-must-not-migrate" | Set-Content -LiteralPath (Join-Path $legacy ".furina/voice/sample.wav") -Encoding ascii
"web-cache-must-not-migrate" | Set-Content -LiteralPath (Join-Path $legacy ".furina/web_cache/page.json") -Encoding ascii
"log-must-not-migrate" | Set-Content -LiteralPath (Join-Path $legacy "furina.log") -Encoding ascii
"build-output-must-not-migrate" | Set-Content -LiteralPath (Join-Path $legacy "target/output.txt") -Encoding ascii
"dialogue_style: rc_fixture" | Set-Content -LiteralPath (Join-Path $legacy "persona/furina.yaml") -Encoding utf8
"# Desktop marker only" | Set-Content -LiteralPath (Join-Path $legacy "python/furina_tools/server.py") -Encoding utf8
Write-SyntheticVrm (Join-Path $legacy ".furina/avatar/Furina.vrm")

"cli: true" | Set-Content -LiteralPath (Join-Path $cli ".furina/config.yaml") -Encoding utf8
"CLI memory must never migrate" | Set-Content -LiteralPath (Join-Path $cli ".furina/memory/cli.txt") -Encoding utf8
"This directory intentionally lacks Desktop persona and Python markers." | Set-Content -LiteralPath (Join-Path $cli "README.txt") -Encoding utf8

"Furina RC sidecar read-only fixture" | Set-Content -LiteralPath (Join-Path $workspace "README.txt") -Encoding utf8
"not a vrm" | Set-Content -LiteralPath (Join-Path $output "invalid-avatar.vrm") -Encoding ascii
"wrong extension" | Set-Content -LiteralPath (Join-Path $output "wrong-extension.txt") -Encoding ascii
Write-SyntheticVrm (Join-Path $output "migration-only-synthetic.vrm")

@'
IMPORTANT:
- migration-only-synthetic.vrm is only for migration and parser checks.
- It is not a complete visual Avatar and must not be used for hair, blink, mouth, or expression acceptance.
- Supply your own legally licensed visual VRM inside Windows Sandbox for visual testing.
- All secrets in this fixture are dummy values.
'@ | Set-Content -LiteralPath (Join-Path $output "README.txt") -Encoding utf8

Write-Output "fixtures=$output"
