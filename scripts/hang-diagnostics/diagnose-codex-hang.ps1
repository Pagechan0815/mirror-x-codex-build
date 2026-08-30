param()

$ErrorActionPreference = 'Continue'
Set-StrictMode -Version Latest

function Mask-Secrets {
    param([AllowNull()][string]$Text)
    if ($null -eq $Text) { return '' }
    $masked = $Text
    $masked = $masked -replace '(?i)Bearer\s+[A-Za-z0-9._~+/\-=]+', 'Bearer <redacted>'
    $masked = $masked -replace '(?i)sk-[A-Za-z0-9_-]{8,}', '<redacted-key>'
    $masked = $masked -replace '(?i)("(?:access_token|refresh_token|id_token|OPENAI_API_KEY|experimental_bearer_token)"\s*:\s*")[^"]+(")', '$1<redacted>$2'
    return $masked
}

function Get-TomlRootString {
    param([string]$Text, [string]$Name)
    $inSection = $false
    foreach ($line in ($Text -split "`r?`n")) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[') { $inSection = $true }
        if (-not $inSection -and $trimmed -match ('^' + [regex]::Escape($Name) + '\s*=\s*["'']([^"'']*)["'']')) {
            return $Matches[1]
        }
    }
    return ''
}

function Get-TomlProviderString {
    param([string]$Text, [string]$Provider, [string]$Name)
    if ([string]::IsNullOrWhiteSpace($Provider)) { return '' }
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

function Read-AuthMetadata {
    param([string]$Path)
    $result = [ordered]@{
        ApiKey = ''
        AuthMode = ''
        HasTokens = $false
        ParseOk = $false
    }
    if (-not (Test-Path -LiteralPath $Path)) { return $result }
    try {
        $json = Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
        $result.ParseOk = $true
        if ($null -ne $json.OPENAI_API_KEY) { $result.ApiKey = [string]$json.OPENAI_API_KEY }
        if ($null -ne $json.auth_mode) { $result.AuthMode = [string]$json.auth_mode }
        if ($null -ne $json.tokens) { $result.HasTokens = $true }
    } catch {}
    return $result
}

function Invoke-Probe {
    param(
        [string]$Name,
        [string]$Uri,
        [string]$Method,
        [string]$Token,
        [AllowNull()][string]$Body,
        [int]$TimeoutSec
    )
    $started = Get-Date
    try {
        $headers = @{ Authorization = "Bearer $Token" }
        $params = @{
            Uri = $Uri
            Method = $Method
            Headers = $headers
            TimeoutSec = $TimeoutSec
            UseBasicParsing = $true
        }
        if ($null -ne $Body) {
            $params.ContentType = 'application/json'
            $params.Body = $Body
        }
        $response = Invoke-WebRequest @params
        $elapsed = [int]((Get-Date) - $started).TotalMilliseconds
        Write-Output "$Name status=$([int]$response.StatusCode) elapsed_ms=$elapsed bytes=$($response.RawContentLength)"
    } catch {
        $elapsed = [int]((Get-Date) - $started).TotalMilliseconds
        $status = 'none'
        $requestId = ''
        if ($null -ne $_.Exception.Response) {
            try {
                $status = [int]$_.Exception.Response.StatusCode
                $requestId = [string]$_.Exception.Response.Headers['x-request-id']
            } catch {}
        }
        Write-Output "$Name status=$status elapsed_ms=$elapsed request_id=$requestId error=$(Mask-Secrets $_.Exception.Message)"
    }
}

$report = Join-Path $env:USERPROFILE 'Desktop\mirror-codex-hang-report.txt'
Start-Transcript -LiteralPath $report -Force | Out-Null

Write-Output '=== Mirror X Codex hang diagnostics ==='
Write-Output "local_time=$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss zzz')"

$codexHome = if ([string]::IsNullOrWhiteSpace($env:CODEX_HOME)) {
    Join-Path $HOME '.codex'
} else {
    $env:CODEX_HOME
}
$configPath = Join-Path $codexHome 'config.toml'
$authPath = Join-Path $codexHome 'auth.json'
$configText = if (Test-Path -LiteralPath $configPath) {
    Get-Content -LiteralPath $configPath -Raw
} else {
    ''
}
$provider = Get-TomlRootString -Text $configText -Name 'model_provider'
$model = Get-TomlRootString -Text $configText -Name 'model'
$baseUrl = Get-TomlProviderString -Text $configText -Provider $provider -Name 'base_url'
$providerToken = Get-TomlProviderString -Text $configText -Provider $provider -Name 'experimental_bearer_token'
$auth = Read-AuthMetadata -Path $authPath
$runtimeToken = if (-not [string]::IsNullOrWhiteSpace($providerToken)) {
    $providerToken
} else {
    [string]$auth.ApiKey
}

Write-Output "CODEX_HOME=$codexHome"
Write-Output "provider=$provider"
Write-Output "model=$model"
Write-Output "base_url=$baseUrl"
Write-Output "config_bearer_present=$(-not [string]::IsNullOrWhiteSpace($providerToken))"
Write-Output "auth_api_key_present=$(-not [string]::IsNullOrWhiteSpace([string]$auth.ApiKey))"
Write-Output "auth_mode=$($auth.AuthMode)"
Write-Output "auth_tokens_present=$($auth.HasTokens)"

Write-Output '--- Processes ---'
$allProcesses = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue
$codexProcesses = @($allProcesses | Where-Object {
    $_.Name -match '(?i)codex' -or $_.ExecutablePath -match '(?i)\\Codex\\|OpenAI\.Codex'
})
foreach ($process in $codexProcesses) {
    $runtime = Get-Process -Id $process.ProcessId -ErrorAction SilentlyContinue
    $cpu = if ($null -ne $runtime) { $runtime.CPU } else { $null }
    $memory = if ($null -ne $runtime) { $runtime.WorkingSet64 } else { $null }
    Write-Output "pid=$($process.ProcessId) parent=$($process.ParentProcessId) name=$($process.Name) cpu_s=$cpu memory_bytes=$memory path=$($process.ExecutablePath)"
}
if ($codexProcesses.Count -eq 0) { Write-Output 'no_codex_process=true' }

Write-Output '--- TCP connections ---'
foreach ($process in $codexProcesses) {
    Get-NetTCPConnection -OwningProcess $process.ProcessId -ErrorAction SilentlyContinue |
        Where-Object { $_.State -in @('Established', 'SynSent', 'CloseWait') } |
        ForEach-Object {
            Write-Output "pid=$($_.OwningProcess) state=$($_.State) local=$($_.LocalAddress):$($_.LocalPort) remote=$($_.RemoteAddress):$($_.RemotePort)"
        }
}

Write-Output '--- Direct API probes ---'
if ([string]::IsNullOrWhiteSpace($runtimeToken) -or [string]::IsNullOrWhiteSpace($baseUrl)) {
    Write-Output 'probe_skipped=missing_runtime_token_or_base_url'
} else {
    $modelsUri = $baseUrl.TrimEnd('/') + '/models'
    Invoke-Probe -Name 'models_probe' -Uri $modelsUri -Method 'Get' -Token $runtimeToken -Body $null -TimeoutSec 20
    if ([string]::IsNullOrWhiteSpace($model)) {
        Write-Output 'responses_probe_skipped=missing_model'
    } else {
        $responsesUri = $baseUrl.TrimEnd('/') + '/responses'
        $normalBody = @{
            model = $model
            input = 'Reply only: OK'
            max_output_tokens = 16
            stream = $false
        } | ConvertTo-Json -Compress
        Invoke-Probe -Name 'responses_nonstream_probe' -Uri $responsesUri -Method 'Post' -Token $runtimeToken -Body $normalBody -TimeoutSec 90

        $streamBody = @{
            model = $model
            input = 'Reply only: OK'
            max_output_tokens = 16
            stream = $true
        } | ConvertTo-Json -Compress
        Invoke-Probe -Name 'responses_stream_probe' -Uri $responsesUri -Method 'Post' -Token $runtimeToken -Body $streamBody -TimeoutSec 60
    }
}

Write-Output '--- Recent Codex log metadata ---'
$logRoot = Join-Path $env:LOCALAPPDATA 'Packages\OpenAI.Codex_2p2nqsd0c76g0\LocalCache\Local\Codex\Logs'
$recentLogs = @()
if (Test-Path -LiteralPath $logRoot) {
    $recentLogs = @(Get-ChildItem -LiteralPath $logRoot -Recurse -File -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 5)
}
foreach ($log in $recentLogs) {
    Write-Output "log=$($log.FullName) bytes=$($log.Length) modified=$($log.LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss'))"
}

Write-Output '--- Recent relevant log lines ---'
foreach ($log in $recentLogs) {
    try {
        Get-Content -LiteralPath $log.FullName -Tail 500 -ErrorAction Stop |
            Where-Object { $_ -match '(?i)error|warning|unauthorized|invalid token|timeout|timed out|stream|disconnect|responses|request.?id|failed' } |
            Select-Object -Last 100 |
            ForEach-Object { Write-Output (Mask-Secrets $_) }
    } catch {}
}

$mirrorLog = Join-Path $HOME '.mirrorplus\codex-plus.log'
if (Test-Path -LiteralPath $mirrorLog) {
    Write-Output '--- Recent Mirror X log lines ---'
    Get-Content -LiteralPath $mirrorLog -Tail 300 -ErrorAction SilentlyContinue |
        Where-Object { $_ -match '(?i)error|warning|failed|timeout|stream|proxy|relay|launch' } |
        Select-Object -Last 100 |
        ForEach-Object { Write-Output (Mask-Secrets $_) }
}

Write-Output '=== End ==='
Stop-Transcript | Out-Null
Write-Host ''
Write-Host "Report saved to $report"

