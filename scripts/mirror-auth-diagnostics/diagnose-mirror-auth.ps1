param(
    [switch]$ProbeResponses
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Get-ShortFingerprint {
    param([AllowNull()][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) {
        return '<missing>'
    }
    $bytes = [Text.Encoding]::UTF8.GetBytes($Value.Trim())
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $hash = $sha.ComputeHash($bytes)
    } finally {
        $sha.Dispose()
    }
    return (([BitConverter]::ToString($hash) -replace '-', '').Substring(0, 12)).ToLowerInvariant()
}

function Get-TomlRootString {
    param([string]$Text, [string]$Name)
    $inSection = $false
    foreach ($line in ($Text -split "`r?`n")) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[') {
            $inSection = $true
        }
        if (-not $inSection -and $trimmed -match ('^' + [regex]::Escape($Name) + '\s*=\s*["'']([^"'']*)["'']')) {
            return $Matches[1]
        }
    }
    return ''
}

function Get-TomlProviderString {
    param([string]$Text, [string]$Provider, [string]$Name)
    if ([string]::IsNullOrWhiteSpace($Provider)) {
        return ''
    }
    $wanted = "model_providers.$Provider"
    $section = ''
    foreach ($line in ($Text -split "`r?`n")) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[([^\]]+)\]') {
            $section = $Matches[1].Trim()
            continue
        }
        if ($section -eq $wanted -and $trimmed -match ('^' + [regex]::Escape($Name) + '\s*=\s*["'']([^"'']*)["'']')) {
            return $Matches[1]
        }
    }
    return ''
}

function Get-JsonApiKey {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) {
        return ''
    }
    try {
        $json = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
        if ($null -ne $json.OPENAI_API_KEY) {
            return [string]$json.OPENAI_API_KEY
        }
    } catch {
        return ''
    }
    return ''
}

function Get-EnvironmentValue {
    param([string]$Scope)
    return [Environment]::GetEnvironmentVariable('OPENAI_API_KEY', $Scope)
}

$codexHome = if ([string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
    Join-Path $HOME '.codex'
} else {
    $env:CODEX_HOME
}
$configPath = Join-Path $codexHome 'config.toml'
$authPath = Join-Path $codexHome 'auth.json'
$settingsPath = Join-Path $HOME '.mirrorplus\settings.json'

$configText = if (Test-Path -LiteralPath $configPath) {
    Get-Content -LiteralPath $configPath -Raw
} else {
    ''
}
$provider = Get-TomlRootString -Text $configText -Name 'model_provider'
$model = Get-TomlRootString -Text $configText -Name 'model'
$baseUrl = Get-TomlProviderString -Text $configText -Provider $provider -Name 'base_url'
$providerToken = Get-TomlProviderString -Text $configText -Provider $provider -Name 'experimental_bearer_token'
$authToken = Get-JsonApiKey -Path $authPath
$processToken = Get-EnvironmentValue -Scope 'Process'
$userToken = Get-EnvironmentValue -Scope 'User'
$machineToken = Get-EnvironmentValue -Scope 'Machine'

Write-Output '=== Mirror X Codex auth diagnostics (secrets are never printed) ==='
Write-Output "CODEX_HOME: $codexHome"
Write-Output "config.toml: $(Test-Path -LiteralPath $configPath)"
Write-Output "auth.json: $(Test-Path -LiteralPath $authPath)"
Write-Output "settings.json: $(Test-Path -LiteralPath $settingsPath)"
Write-Output "model_provider: $provider"
Write-Output "model: $model"
Write-Output "base_url: $baseUrl"
Write-Output "config bearer fingerprint: $(Get-ShortFingerprint $providerToken)"
Write-Output "auth.json key fingerprint: $(Get-ShortFingerprint $authToken)"
Write-Output "process env fingerprint: $(Get-ShortFingerprint $processToken)"
Write-Output "user env fingerprint: $(Get-ShortFingerprint $userToken)"
Write-Output "machine env fingerprint: $(Get-ShortFingerprint $machineToken)"

$codexProcesses = Get-CimInstance Win32_Process -Filter "Name='Codex.exe' OR Name='codex.exe'" -ErrorAction SilentlyContinue
if ($codexProcesses) {
    foreach ($process in $codexProcesses) {
        Write-Output "Codex PID=$($process.ProcessId) Path=$($process.ExecutablePath)"
    }
} else {
    Write-Output 'Codex process: not running'
}

if (-not $ProbeResponses) {
    Write-Output 'Responses probe: skipped (use -ProbeResponses to run it)'
    exit 0
}

$runtimeToken = if (-not [string]::IsNullOrWhiteSpace($providerToken)) {
    $providerToken
} else {
    $authToken
}
if ([string]::IsNullOrWhiteSpace($runtimeToken)) {
    Write-Output 'Responses probe: cannot run; final config has no token'
    exit 2
}
if ([string]::IsNullOrWhiteSpace($baseUrl)) {
    Write-Output 'Responses probe: cannot run; final config has no base_url'
    exit 2
}
if ([string]::IsNullOrWhiteSpace($model)) {
    Write-Output 'Responses probe: cannot run; final config has no model'
    exit 2
}

$endpoint = $baseUrl.TrimEnd('/') + '/responses'
$headers = @{
    Authorization = "Bearer $runtimeToken"
    'Content-Type' = 'application/json'
}
$body = @{
    model = $model
    input = 'Reply only: OK'
    max_output_tokens = 16
    stream = $false
} | ConvertTo-Json -Compress

try {
    $response = Invoke-WebRequest -Uri $endpoint -Method Post -Headers $headers -Body $body -TimeoutSec 90
    Write-Output "Responses probe: HTTP $([int]$response.StatusCode) success"
} catch {
    $status = $null
    $requestId = ''
    if ($null -ne $_.Exception.Response) {
        try {
            $status = [int]$_.Exception.Response.StatusCode
            $requestId = [string]$_.Exception.Response.Headers['x-request-id']
        } catch {}
    }
    if ($null -eq $status) {
        Write-Output "Responses probe: connection failed: $($_.Exception.Message)"
    } else {
        Write-Output "Responses probe: HTTP $status failed request_id=$requestId"
    }
    exit 3
}
