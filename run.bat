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
echo Build the Windows app first with tools\build_app.ps1. 1>&2
exit /b 1

:run
"%bin%jumpbot.exe" %*
exit /b %errorlevel%
