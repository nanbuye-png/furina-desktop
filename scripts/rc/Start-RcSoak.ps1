[CmdletBinding()]
param(
    [int]$DurationMinutes = 120,
    [int]$SampleSeconds = 30,
    [string]$ProcessName = "furina-desktop",
    [int]$ProcessId = 0,
    [string]$OutputDirectory = "C:\FurinaRcResults"
)

$ErrorActionPreference = "Stop"
if ($DurationMinutes -lt 1) { throw "DurationMinutes 必须大于 0" }
if ($SampleSeconds -lt 5) { throw "SampleSeconds 必须至少为 5" }
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$process = if ($ProcessId -gt 0) { Get-Process -Id $ProcessId -ErrorAction SilentlyContinue } else { Get-Process -Name $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1 }
if (-not $process) { throw "未找到目标进程，请先启动 Furina Desktop" }
$pidToWatch = $process.Id
$startedAt = Get-Date
$deadline = $startedAt.AddMinutes($DurationMinutes)
$samples = [System.Collections.Generic.List[object]]::new()
$processExited = $false
$lastCheckpoint = -1
Write-Host "RC soak started. PID=$pidToWatch, duration=$DurationMinutes minutes."
while ((Get-Date) -lt $deadline) {
    $now = Get-Date
    $elapsed = [math]::Floor(($now - $startedAt).TotalMinutes)
    $checkpoint = [math]::Floor($elapsed / 15)
    if ($checkpoint -gt $lastCheckpoint) {
        $lastCheckpoint = $checkpoint
        Write-Host "[$($now.ToString('HH:mm:ss'))] Checkpoint: run chat + ASR + TTS + interrupt + Avatar check."
        if ($elapsed -gt 0 -and ($elapsed % 30) -eq 0) { Write-Host "Also run one approved read-only sidecar call." }
        if ($elapsed -eq 30 -or $elapsed -eq 90) { Write-Host "Also perform one runtime hot reload." }
        if ($elapsed -eq 60) { Write-Host "Also perform one Avatar reload." }
    }
    $current = Get-Process -Id $pidToWatch -ErrorAction SilentlyContinue
    if (-not $current) { $processExited = $true; break }
    $samples.Add([pscustomobject]@{
        timestamp = $now.ToString("o")
        elapsedMinutes = [math]::Round(($now - $startedAt).TotalMinutes, 2)
        pid = $pidToWatch
        workingSetMiB = [math]::Round($current.WorkingSet64 / 1MB, 2)
        privateMemoryMiB = [math]::Round($current.PrivateMemorySize64 / 1MB, 2)
        cpuSeconds = [math]::Round($current.CPU, 2)
        handles = $current.HandleCount
        threads = $current.Threads.Count
        responding = $current.Responding
    })
    Start-Sleep -Seconds $SampleSeconds
}
$endedAt = Get-Date
$samples | Export-Csv -LiteralPath (Join-Path $OutputDirectory "soak-samples.csv") -NoTypeInformation -Encoding utf8
$working = @($samples | ForEach-Object { $_.workingSetMiB })
$private = @($samples | ForEach-Object { $_.privateMemoryMiB })
$summary = [ordered]@{
    startedAt = $startedAt.ToString("o")
    endedAt = $endedAt.ToString("o")
    requestedMinutes = $DurationMinutes
    elapsedMinutes = [math]::Round(($endedAt - $startedAt).TotalMinutes, 2)
    sampleSeconds = $SampleSeconds
    sampleCount = $samples.Count
    processId = $pidToWatch
    processExited = $processExited
    nonRespondingSamples = @($samples | Where-Object { -not $_.responding }).Count
    workingSetStartMiB = if ($working.Count) { $working[0] } else { $null }
    workingSetEndMiB = if ($working.Count) { $working[-1] } else { $null }
    workingSetMaxMiB = if ($working.Count) { ($working | Measure-Object -Maximum).Maximum } else { $null }
    privateMemoryStartMiB = if ($private.Count) { $private[0] } else { $null }
    privateMemoryEndMiB = if ($private.Count) { $private[-1] } else { $null }
    privateMemoryMaxMiB = if ($private.Count) { ($private | Measure-Object -Maximum).Maximum } else { $null }
    passed = -not $processExited -and ($endedAt - $startedAt).TotalMinutes -ge ($DurationMinutes - 0.2)
}
$summary | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $OutputDirectory "soak-summary.json") -Encoding utf8
$summary | Format-List
if (-not $summary.passed) { exit 1 }
