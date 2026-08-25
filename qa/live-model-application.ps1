param(
    [Parameter(Mandatory = $false)]
    [string]$BaseUrl = $env:AWORKIT_LIVE_QA_BASE_URL,

    [Parameter(Mandatory = $false)]
    [string]$Model = $env:AWORKIT_LIVE_QA_MODEL,

    [switch]$KeepArtifacts
)

$ErrorActionPreference = 'Stop'
$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$manifest = Join-Path $repositoryRoot 'desktop\src-tauri\Cargo.toml'
$binary = Join-Path $repositoryRoot 'desktop\src-tauri\target\debug\aworkit-desktop.exe'

if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
    throw 'Set AWORKIT_LIVE_QA_BASE_URL or pass -BaseUrl.'
}
if ([string]::IsNullOrWhiteSpace($Model)) {
    throw 'Set AWORKIT_LIVE_QA_MODEL or pass -Model.'
}

$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$qaRoot = Join-Path $temporaryRoot ('aworkit-live-model-qa-' + [guid]::NewGuid().ToString('N'))
$dataRoot = Join-Path $qaRoot 'data'
$projectRoot = Join-Path $qaRoot 'project'
New-Item -ItemType Directory -Path $dataRoot, $projectRoot | Out-Null

try {
    Write-Host '== Build the actual Aworkit desktop application =='
    & cargo build --manifest-path $manifest --bin aworkit-desktop
    if ($LASTEXITCODE -ne 0) {
        throw "Aworkit desktop build failed with exit code $LASTEXITCODE."
    }

    Write-Host '== Run the actual application against the live model and every built-in tool =='
    & $binary --live-model-qa $dataRoot $projectRoot $BaseUrl $Model
    if ($LASTEXITCODE -ne 0) {
        throw "Aworkit live-model application QA failed with exit code $LASTEXITCODE. Artifacts: $qaRoot"
    }
    Write-Host '== Aworkit live-model application QA: PASS =='
}
finally {
    if ($KeepArtifacts) {
        Write-Host "Live QA artifacts retained at $qaRoot"
    }
    else {
        $resolvedQaRoot = [IO.Path]::GetFullPath($qaRoot)
        if (-not $resolvedQaRoot.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove live QA directory outside the system temporary root: $resolvedQaRoot"
        }
        if (Test-Path -LiteralPath $resolvedQaRoot) {
            Remove-Item -LiteralPath $resolvedQaRoot -Recurse -Force
        }
    }
}
