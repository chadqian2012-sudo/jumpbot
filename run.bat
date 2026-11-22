@echo off
setlocal
pushd "%~dp0" || exit /b 1
set "bin=%~dp0"
if exist "%bin%jumpbot.exe" goto run
set "bin=%~dp0desktop\target\x86_64-pc-windows-msvc\release\"
if exist "%bin%jumpbot.exe" goto run
set "bin=%~dp0desktop\target\release\"
if exist "%bin%jumpbot.exe" goto run
set "bin=%~dp0desktop\target\portable\x86_64-pc-windows-msvc\release\"
if exist "%bin%jumpbot.exe" goto run
if not exist "%~dp0jumpbot.py" (
    echo Build the Windows app first with tools\build_app.ps1. 1>&2
    exit /b 1
)
set "py=python"
if exist "%~dp0..\.venv\Scripts\python.exe" set "py=%~dp0..\.venv\Scripts\python.exe"
if exist "%~dp0.venv\Scripts\python.exe" set "py=%~dp0.venv\Scripts\python.exe"
if defined VIRTUAL_ENV if exist "%VIRTUAL_ENV%\Scripts\python.exe" set "py=%VIRTUAL_ENV%\Scripts\python.exe"
"%py%" "%~dp0jumpbot.py" %*
exit /b %errorlevel%

:run
"%bin%jumpbot.exe" %*
exit /b %errorlevel%
