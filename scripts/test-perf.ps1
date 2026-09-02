<#
.SYNOPSIS
Lightweight performance sampling against a deployed RSS Worker.
Run with: pwsh scripts/test-perf.ps1 [-Base https://...] [-Iterations 30]
#>
param(
    [string]$Base = "https://rss-worker.weixc0856.workers.dev",
    [int]$Iterations = 30
)

$ProgressPreference = "SilentlyContinue"

function Sample {
    param([string]$Name, [scriptblock]$Call)
    $times = @()
    for ($i = 0; $i -lt $Iterations; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        try { & $Call | Out-Null; $sw.Stop(); $times += $sw.Elapsed.TotalMilliseconds }
        catch { $sw.Stop(); $times += $sw.Elapsed.TotalMilliseconds }
    }
    $sorted = $times | Sort-Object
    $avg = ($sorted | Measure-Object -Average).Average
    $p95 = $sorted[[int]($sorted.Count * 0.95)]
    $max = $sorted[-1]
    "{0,-16} n={1,-4} avg={2,8:N1}ms  p95={3,8:N1}ms  max={4,8:N1}ms" -f $Name, $times.Count, $avg, $p95, $max
}

Write-Host "== Performance sampling ($Iterations iterations, base=$Base) =="
Sample "GET /health" { Invoke-WebRequest -Uri "$Base/health" -UseBasicParsing -TimeoutSec 20 }
Sample "GET /api/diagnostics" { Invoke-WebRequest -Uri "$Base/api/diagnostics" -UseBasicParsing -TimeoutSec 20 }
Sample "GET /api/sources" { Invoke-WebRequest -Uri "$Base/api/sources" -Headers @{ "X-User-Id" = "perf" } -UseBasicParsing -TimeoutSec 20 }
Sample "GET /api/feeds" { Invoke-WebRequest -Uri "$Base/api/feeds" -UseBasicParsing -TimeoutSec 20 }

# Articles read (requires a demo source with articles; demo id 1 exists on dev)
try {
    $arts = Invoke-WebRequest -Uri "$Base/api/sources/1/articles" -Headers @{ "X-User-Id" = "demo" } -UseBasicParsing -TimeoutSec 30
    $n = (($arts.Content | ConvertFrom-Json).data).Count
    Write-Host "source articles available: $n (sampling read path below)"
    Sample "GET source articles" { Invoke-WebRequest -Uri "$Base/api/sources/1/articles" -Headers @{ "X-User-Id" = "demo" } -UseBasicParsing -TimeoutSec 30 }
}
catch { Write-Host "note: no demo source articles to sample" }

Write-Host "perf sampling done"
exit 0
