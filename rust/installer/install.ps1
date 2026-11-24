# Instala ou atualiza o Spark a partir do ultimo release publico no GitHub.
# Uso: irm https://github.com/mrodriguesh/Spark/releases/latest/download/install.ps1 | iex

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = 'mrodriguesh/Spark'
$WebView2Guid = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'

Write-Host ''
Write-Host '  =============================================' -ForegroundColor Magenta
Write-Host '             SPARK - INSTALADOR OFICIAL        ' -ForegroundColor Magenta
Write-Host '  =============================================' -ForegroundColor Magenta
Write-Host ''

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'O Spark requer Windows 10 ou 11 de 64 bits.'
}

function Test-WebView2Installed {
    $paths = @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$WebView2Guid",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$WebView2Guid",
        "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$WebView2Guid"
    )
    foreach ($p in $paths) {
        if (Test-Path $p) {
            $pv = (Get-ItemProperty -Path $p -Name 'pv' -ErrorAction SilentlyContinue).pv
            if ($pv -and $pv -ne '0.0.0.0') { return $true }
        }
    }
    return $false
}

if (-not (Test-WebView2Installed)) {
    Write-Host '[1/3] Microsoft Edge WebView2 Runtime nao encontrado. Instalando...' -ForegroundColor Yellow
    $bootstrapper = Join-Path $env:TEMP 'MicrosoftEdgeWebview2Setup.exe'
    Invoke-WebRequest -Uri 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' -OutFile $bootstrapper -UseBasicParsing
    $proc = Start-Process -FilePath $bootstrapper -ArgumentList '/silent', '/install' -Wait -PassThru
    Remove-Item $bootstrapper -Force -ErrorAction SilentlyContinue
    if ($proc.ExitCode -ne 0 -and -not (Test-WebView2Installed)) {
        throw 'Nao foi possivel instalar o Microsoft Edge WebView2 Runtime automaticamente.'
    }
    Write-Host '[OK] WebView2 Runtime instalado.' -ForegroundColor Green
} else {
    Write-Host '[1/3] Microsoft Edge WebView2 Runtime detectado.' -ForegroundColor Green
}

Write-Host '[2/3] Buscando a versao mais recente do Spark no GitHub...' -ForegroundColor Cyan
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ 'User-Agent' = 'Spark-Installer' }
$exeAsset = $release.assets | Where-Object { $_.name -match '^Spark-Setup-.*\.exe$' } | Select-Object -First 1
$sumAsset = $release.assets | Where-Object { $_.name -eq 'SHA256SUMS.txt' } | Select-Object -First 1

if (-not $exeAsset) {
    throw "Instalador nao encontrado no release $($release.tag_name)."
}

$workDir = Join-Path $env:TEMP "Spark-Install-$PID"
New-Item -ItemType Directory -Path $workDir -Force | Out-Null

try {
    $installerPath = Join-Path $workDir $exeAsset.name
    Write-Host "      Baixando $($exeAsset.name) ($([math]::Round($exeAsset.size / 1MB, 2)) MB)..." -ForegroundColor Cyan
    Invoke-WebRequest -Uri $exeAsset.browser_download_url -OutFile $installerPath -UseBasicParsing

    if ($sumAsset) {
        $sumsPath = Join-Path $workDir 'SHA256SUMS.txt'
        Invoke-WebRequest -Uri $sumAsset.browser_download_url -OutFile $sumsPath -UseBasicParsing
        $expectedLine = Get-Content $sumsPath | Where-Object { $_ -match [regex]::Escape($exeAsset.name) } | Select-Object -First 1
        if ($expectedLine) {
            $expectedHash = ($expectedLine -split '\s+')[0].Trim().ToLowerInvariant()
            $actualHash = (Get-FileHash -Path $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($expectedHash -ne $actualHash) {
                throw "Falha na verificacao SHA-256! Esperado: $expectedHash | Obtido: $actualHash"
            }
            Write-Host '      Integridade SHA-256 verificada com sucesso.' -ForegroundColor Green
        }
    }

    Write-Host '[3/3] Executando o instalador do Spark...' -ForegroundColor Cyan
    Start-Process -FilePath $installerPath -Wait
    Write-Host ''
    Write-Host '[OK] Spark instalado com sucesso! Bom uso.' -ForegroundColor Green
} finally {
    Remove-Item $workDir -Recurse -Force -ErrorAction SilentlyContinue
}
