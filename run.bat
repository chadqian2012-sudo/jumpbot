@echo off
setlocal
chcp 65001 >nul
cd /d "%~dp0"

set "PY=%~dp0..\.venv\Scripts\python.exe"
if not exist "%PY%" (
  echo [错误] 找不到虚拟环境解释器: %PY%
  echo 请先按 README.md 的“环境”一节创建 .venv 并安装依赖。
  pause
  exit /b 1
)

echo 启动 跳一跳 自动脚本（默认前台模式，F8 停止）...
"%PY%" "%~dp0jumpbot.py" %*
set "EXITCODE=%ERRORLEVEL%"
echo.
echo 退出码: %EXITCODE%
pause
