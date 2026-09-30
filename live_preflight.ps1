# Read-only sibling inspection; never starts/stops a trader or reads its secrets.
$ErrorActionPreference = 'Stop'
$projectRoot = $PSScriptRoot
$xemmRoot = Join-Path (Split-Path $projectRoot -Parent) 'XEMM\CROSS_EXCHANGE_MARKET_MAKING_LIGHTER_ASTER\LIGHTER_ASTER_BOT'
$rawStatus = docker compose --project-directory $xemmRoot ps --all --format json
if ($LASTEXITCODE -ne 0) { throw 'Cannot verify XEMM container state' }
$rows = @($rawStatus | ForEach-Object { $_ | ConvertFrom-Json })
$activeLive = @($rows | Where-Object { $_.Service -eq 'bot' -and $_.State -eq 'running' })
$proof = @{
    checked_utc_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    source = $xemmRoot
    inactive = ($activeLive.Count -eq 0)
    active_live_names = @($activeLive | ForEach-Object { $_.Name })
}
$proofDirectory = Join-Path $projectRoot 'runs\live'
[System.IO.Directory]::CreateDirectory($proofDirectory) | Out-Null
$proofPath = Join-Path $proofDirectory 'xemm-status.json'
[System.IO.File]::WriteAllText($proofPath, ($proof | ConvertTo-Json), [System.Text.UTF8Encoding]::new($false))
if (!$proof.inactive) { throw 'XEMM live trading is active; live validation is blocked' }
Write-Output 'XEMM live trading inactive; dry runs and recorder untouched.'
