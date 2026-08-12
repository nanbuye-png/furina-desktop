[CmdletBinding()]
param(
    [string[]]$ResultDirectory = @("C:\FurinaRcResults"),
    [switch]$RequireSoak
)

$ErrorActionPreference = "Stop"
if ($ResultDirectory.Count -eq 1 -and $ResultDirectory[0].Contains(",")) { $ResultDirectory = @($ResultDirectory[0].Split(",", [System.StringSplitOptions]::RemoveEmptyEntries)) }
$checklist = @()
$issues = @()
foreach ($directory in $ResultDirectory) {
    $checklistPath = Join-Path $directory "rc-checklist.csv"
    $issuesPath = Join-Path $directory "rc-issues.csv"
    if (-not (Test-Path $checklistPath)) { throw "缺少验收清单: $checklistPath" }
    if (-not (Test-Path $issuesPath)) { throw "缺少问题记录: $issuesPath" }
    $checklist += @(Import-Csv -LiteralPath $checklistPath)
    $issues += @(Import-Csv -LiteralPath $issuesPath)
}
$requiredNotPassed = @($checklist | Where-Object { $_.Required -eq "yes" -and $_.Status.ToUpperInvariant() -ne "PASS" })
$blockingIssues = @($issues | Where-Object { $_.Status.ToUpperInvariant() -ne "CLOSED" -and $_.Severity.ToUpperInvariant() -in @("P0", "P1") })
$undispositionedP2 = @($issues | Where-Object { $_.Status.ToUpperInvariant() -ne "CLOSED" -and $_.Severity.ToUpperInvariant() -eq "P2" -and [string]::IsNullOrWhiteSpace($_.Disposition) })
$soakPassed = $true
$soakSummary = $null
$soakPaths = @($ResultDirectory | ForEach-Object { Join-Path $_ "soak-summary.json" })
$soakPath = $soakPaths | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($RequireSoak) {
    if (-not $soakPath -or -not (Test-Path -LiteralPath $soakPath)) { $soakPassed = $false } else {
        $soakSummary = Get-Content -LiteralPath $soakPath -Raw | ConvertFrom-Json
        $soakPassed = [bool]$soakSummary.passed -and [double]$soakSummary.elapsedMinutes -ge 119.8
    }
}
$passed = $requiredNotPassed.Count -eq 0 -and $blockingIssues.Count -eq 0 -and $undispositionedP2.Count -eq 0 -and $soakPassed
$report = [ordered]@{
    generatedAt = (Get-Date).ToString("o")
    passed = $passed
    requiredNotPassed = $requiredNotPassed
    blockingIssues = $blockingIssues
    undispositionedP2 = $undispositionedP2
    soakRequired = [bool]$RequireSoak
    soakPassed = $soakPassed
    soakSummary = $soakSummary
}
$gateOutputRoot = $ResultDirectory[0]
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $gateOutputRoot "rc-gate.json") -Encoding utf8
Write-Output "required_not_passed=$($requiredNotPassed.Count)"
Write-Output "blocking_issues=$($blockingIssues.Count)"
Write-Output "p2_without_disposition=$($undispositionedP2.Count)"
Write-Output "soak_passed=$soakPassed"
Write-Output "rc_gate_passed=$passed"
if (-not $passed) { exit 1 }
