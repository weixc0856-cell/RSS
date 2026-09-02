<#
.SYNOPSIS
Renders wrangler.toml from wrangler.toml.template by substituting {{KEY}}
placeholders with values gathered from (highest priority first):
  1. process environment variables
  2. .env.production
  3. .env.local

.DESCRIPTION
Keeps real Cloudflare resource IDs out of version control: only the
placeholder template (wrangler.toml.template) is committed, and
wrangler.toml is generated locally/CI and ignored by git.

.EXAMPLE
pwsh scripts/render-config.ps1
pwsh scripts/render-config.ps1 -OutFile wrangler.toml
#>
[CmdletBinding()]
param(
    # Path (relative to the repository root) of the local/development env file.
    [string]$LocalEnvFile = ".env.local",

    # Path (relative to the repository root) of the production env file.
    [string]$ProductionEnvFile = ".env.production",

    # Template file name relative to the repository root.
    [string]$TemplateFile = "wrangler.toml.template",

    # Output file name relative to the repository root.
    [string]$OutFile = "wrangler.toml"
)

$ErrorActionPreference = "Stop"

# scripts/ -> repository root
$RepoRoot = if ($PSScriptRoot) { Split-Path -Parent $PSScriptRoot } else { (Get-Location).Path }

function Read-EnvFile {
    param([string]$Path)

    $map = @{}
    if (-not $Path -or -not (Test-Path -LiteralPath $Path)) {
        return $map
    }

    foreach ($rawLine in Get-Content -LiteralPath $Path) {
        $line = $rawLine.Trim()
        if (-not $line -or $line.StartsWith("#")) {
            continue
        }
        if ($line -match '^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*?)\s*$') {
            $key = $matches[1]
            $value = $matches[2]
            # Strip trailing inline comments ("KEY=value # note") and quotes.
            $value = $value -replace '\s+#.*$', ''
            $value = $value.Trim()
            if ($value.Length -ge 2) {
                $first = $value[0]
                $last = $value[$value.Length - 1]
                if (($first -eq '"' -and $last -eq '"') -or ($first -eq "'" -and $last -eq "'")) {
                    $value = $value.Substring(1, $value.Length - 2)
                }
            }
            if ($null -eq $value) { $value = "" }
            $map[$key] = $value
        }
    }
    return $map
}

# ---------------------------------------------------------------------------
$templatePath = Join-Path $RepoRoot $TemplateFile
if (-not (Test-Path -LiteralPath $templatePath)) {
    throw "Template not found: $templatePath"
}

$localPath = Join-Path $RepoRoot $LocalEnvFile
$prodPath = Join-Path $RepoRoot $ProductionEnvFile

$values = @{}
foreach ($kv in (Read-EnvFile -Path $localPath).GetEnumerator()) {
    $values[$kv.Key] = $kv.Value
}
# Production values win over the shared local file for duplicate keys.
foreach ($kv in (Read-EnvFile -Path $prodPath).GetEnumerator()) {
    $values[$kv.Key] = $kv.Value
}

$templateText = [System.IO.File]::ReadAllText($templatePath)

# Discover every token referenced by the template.
$tokenPattern = '\{\{([A-Za-z_][A-Za-z0-9_]*)\}\}'
$tokenNames = [regex]::Matches($templateText, $tokenPattern) |
    ForEach-Object { $_.Groups[1].Value } |
    Sort-Object -Unique

# Process environment variables take the highest precedence.
foreach ($name in $tokenNames) {
    $envValue = [Environment]::GetEnvironmentVariable($name)
    if ($null -ne $envValue -and $envValue.Length -gt 0) {
        $values[$name] = $envValue
    }
}

$missing = New-Object 'System.Collections.Generic.List[string]'

$resolved = [regex]::Replace($templateText, $tokenPattern, {
    param($match)
    $key = $match.Groups[1].Value
    if ($values.ContainsKey($key) -and $values[$key]) {
        return $values[$key]
    }
    $missing.Add($key) | Out-Null
    return $match.Value
})

if ($missing.Count -gt 0) {
    $list = ($missing | Sort-Object -Unique) -join ", "
    throw "Missing values for placeholder(s): $list. Set them in $LocalEnvFile, $ProductionEnvFile, or export them as environment variables first."
}

$outPath = Join-Path $RepoRoot $OutFile
[System.IO.File]::WriteAllText(
    $outPath,
    $resolved,
    (New-Object System.Text.UTF8Encoding($false))
)

$used = $tokenNames | Where-Object { $values.ContainsKey($_) } | Sort-Object
Write-Host "Rendered '$outPath' from '$TemplateFile'."
Write-Host "Substituted $($used.Count) placeholder(s): $($used -join ', ')"
