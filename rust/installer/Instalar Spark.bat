@echo off
title Spark 2 - Instalador
color 0D
cd /d "%~dp0\..\.."

echo ===================================================
echo               SPARK 2 - INSTALADOR
echo ===================================================
echo.

if exist "rust\dist\Spark Setup 2.0.0.exe" (
    echo Iniciando o instalador do Spark...
    start "" "rust\dist\Spark Setup 2.0.0.exe"
    exit /b 0
)

echo Instalador binario nao encontrado na pasta local.
echo Gerando o instalador a partir do codigo-fonte (Rust + electron-builder)...
echo.

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "BASH_EXE=bash"
where bash >nul 2>nul
if %ERRORLEVEL% NEQ 0 (
    if exist "%ProgramFiles%\Git\bin\bash.exe" (
        set "BASH_EXE=%ProgramFiles%\Git\bin\bash.exe"
    ) else if exist "%LocalAppData%\Programs\Git\bin\bash.exe" (
        set "BASH_EXE=%LocalAppData%\Programs\Git\bin\bash.exe"
    ) else (
        echo Git Bash nao encontrado. Instale o Git for Windows e tente de novo.
        pause
        exit /b 1
    )
)

call npm install --prefix rust\installer
if %ERRORLEVEL% NEQ 0 (
    echo Erro ao instalar dependencias do Node.js.
    pause
    exit /b 1
)

"%BASH_EXE%" rust/installer/build.sh
if %ERRORLEVEL% NEQ 0 (
    echo Erro ao compilar o aplicativo.
    pause
    exit /b 1
)

if exist "rust\dist\Spark Setup 2.0.0.exe" (
    echo.
    echo Instalador gerado com sucesso! Abrindo...
    start "" "rust\dist\Spark Setup 2.0.0.exe"
)
exit /b 0
