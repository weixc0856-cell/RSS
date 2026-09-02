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

# 4. User-scoped isolation + CRUD
$u1 = "t_$(Get-Date -Format 'HHmmssfff')_a"
$u2 = "t_$(Get-Date -Format 'HHmmssfff')_b"
$srcBody = @{ url = "https://rss.nytimes.com/services/xml/rss/nyt/World.xml"; title = "Test NYT $u1"; fetch_interval_minutes = 60 } | ConvertTo-Json -Compress

$created = (Call-Json "/api/sources" $u1 "POST" $srcBody).Content | ConvertFrom-Json
Check "POST /api/sources creates ($u1)" ($created.success -eq $true -and $null -ne $created.data.id)
$sid = $created.data.id

$before = (Call-Json "/api/sources" $u2).Content | ConvertFrom-Json
Check "isolation: $u2 cannot see $u1 source" (@($before.data | Where-Object { $_.id -eq $sid }).Count -eq 0)

$mine = (Call-Json "/api/sources" $u1).Content | ConvertFrom-Json
Check "GET /api/sources lists own source" (@($mine.data | Where-Object { $_.id -eq $sid }).Count -eq 1)

# duplicate create -> conflict
try {
    $dup = Call-Json "/api/sources" $u1 "POST" $srcBody
    Check "duplicate create rejected (status 409)" ($dup.StatusCode -eq 409)
}
catch {
    $code = [int]$_.Exception.Response.StatusCode
    Check "duplicate create rejected (status 409)" ($code -eq 409) "got $code"
}

# update
$up = @{ title = "Test NYT renamed"; fetch_interval_minutes = 15 } | ConvertTo-Json -Compress
$upd = (Call-Json "/api/sources/$sid" $u1 "PUT" $up).Content | ConvertFrom-Json
Check "PUT /api/sources updates" ($upd.success -eq $true)

# manual fetch (integration: worker fetch -> parse -> rss_articles)
$fetch = (Call-Json "/api/sources/$sid/fetch" $u1 "POST").Content | ConvertFrom-Json
Check "POST /api/sources/:id/fetch ok" ($fetch.success -eq $true)
Check "fetch stored articles > 0" ($fetch.data.total -gt 0)

$arts = (Call-Json "/api/sources/$sid/articles" $u1).Content | ConvertFrom-Json
Check "GET source articles non-empty" ($arts.success -eq $true -and $arts.data.Count -gt 0)
if ($arts.data.Count -gt 0) {
    Check "article has title/link/hash" ($arts.data[0].title -and $arts.data[0].link -and $arts.data[0].hash)
}

# isolation on delete & cross-user delete attempt of own source by other user no-op
$otherDel = (Call-Json "/api/sources/$sid" $u2 "DELETE").Content | ConvertFrom-Json
$mineAfter = (Call-Json "/api/sources" $u1).Content | ConvertFrom-Json
Check "delete by non-owner does not remove source" (@($mineAfter.data | Where-Object { $_.id -eq $sid }).Count -eq 1)

$del = (Call-Json "/api/sources/$sid" $u1 "DELETE").Content | ConvertFrom-Json
$mineEnd = (Call-Json "/api/sources" $u1).Content | ConvertFrom-Json
Check "owner delete removes source" (@($mineEnd.data | Where-Object { $_.id -eq $sid }).Count -eq 0)

Write-Host ""
if ($script:fails -gt 0) {
    Write-Host "RESULT: $($script:fails) check(s) FAILED"
    exit 1
}
Write-Host "RESULT: ALL FUNCTIONAL/INTEGRATION CHECKS PASSED"
exit 0
