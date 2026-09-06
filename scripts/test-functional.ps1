<#
.SYNOPSIS
Functional + integration tests against a deployed RSS Worker (HTTP/JSON).
Run with: pwsh scripts/test-functional.ps1 [-Base https://...]
#>
param(
    [string]$Base = "https://rss-worker.weixc0856.workers.dev",
    [int]$Timeout = 120
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$script:fails = 0

function Check {
    param([string]$Name, [bool]$Ok, [string]$Detail = "")
    if ($Ok) { Write-Host "[PASS] $Name" }
    else {
        Write-Host "[FAIL] $Name $Detail"
        $script:fails++
    }
}

function Call-Json {
    param([string]$Path, [string]$User = "demo", [string]$Method = "GET", [string]$Body = "")
    $headers = @{ "X-User-Id" = $User }
    if (-not [string]::IsNullOrEmpty($Body)) {
        $resp = Invoke-WebRequest -Uri ($Base + $Path) -Method $Method -Headers $headers `
            -ContentType "application/json" -Body $Body -UseBasicParsing -TimeoutSec $Timeout
    }
    else {
        $resp = Invoke-WebRequest -Uri ($Base + $Path) -Method $Method -Headers $headers `
            -UseBasicParsing -TimeoutSec $Timeout
    }
    return $resp
}

Write-Host "== Functional / Integration tests against $Base =="

# 1. Health
$h = Call-Json "/health"
Check "GET /health == 200" ($h.StatusCode -eq 200)
Check "GET /health body == ok" ($h.Content -eq "ok")

# 2. Diagnostics shape
$d = (Call-Json "/api/diagnostics").Content | ConvertFrom-Json
Check "GET /api/diagnostics success" ($d.success -eq $true)
Check "diagnostics has feeds_by_status" ($null -ne $d.data.feeds_by_status)
Check "diagnostics has cron_ticks" ($null -ne $d.data.cron_ticks)

# 3. Legacy feeds list
$f = (Call-Json "/api/feeds").Content | ConvertFrom-Json
Check "GET /api/feeds success + array" ($f.success -eq $true -and $f.data -is [array])

# 4. /api/sources is a retired API (dormant prototype layer): GET and POST both
# answer an honest 501 with success:false. No X-User-Id is sent — the endpoint
# must answer the same to everyone and must not touch D1.
foreach ($probe in @(@{ Method = "GET"; Path = "/api/sources"; Body = $null },
                     @{ Method = "POST"; Path = "/api/sources"; Body = '{"url":"https://rss.nytimes.com/services/xml/rss/nyt/World.xml"}' })) {
    $status = 0; $body = $null
    try {
        if ($null -eq $probe.Body) {
            $resp = Invoke-WebRequest -Uri ($Base + $probe.Path) -Method $probe.Method -UseBasicParsing -TimeoutSec $Timeout
        }
        else {
            $resp = Invoke-WebRequest -Uri ($Base + $probe.Path) -Method $probe.Method `
                -ContentType "application/json" -Body $probe.Body -UseBasicParsing -TimeoutSec $Timeout
        }
        $status = $resp.StatusCode; $body = $resp.Content
    }
    catch {
        $status = [int]$_.Exception.Response.StatusCode
        try { $body = $_.ErrorDetails.Message } catch {}
    }
    Check ("{0} {1} == 501" -f $probe.Method, $probe.Path) ($status -eq 501) "got $status"
    if ($body) {
        $j = $body | ConvertFrom-Json
        Check ("{0} {1} success=false" -f $probe.Method, $probe.Path) ($j.success -eq $false)
        Check ("{0} {1} has error text" -f $probe.Method, $probe.Path) (-not [string]::IsNullOrEmpty($j.error))
    }
}

Write-Host ""
if ($script:fails -gt 0) {
    Write-Host "RESULT: $($script:fails) check(s) FAILED"
    exit 1
}
Write-Host "RESULT: ALL FUNCTIONAL/INTEGRATION CHECKS PASSED"
exit 0
