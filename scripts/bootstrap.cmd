@echo off
REM ClaudeRLM bootstrap — downloads the binary on first run.
REM Called from SessionStart hook; all output goes to stderr.
setlocal enabledelayedexpansion

set "PLUGIN_ROOT=%~dp0.."
set "BINARY=%PLUGIN_ROOT%\bin\claude-rlm.exe"
set "REPO=dullfig/claude-rlm"

REM Fast path: binary already exists
if exist "%BINARY%" exit /b 0

echo [claude-rlm] First run — downloading claude-rlm binary... 1>&2

REM Get latest release version and download using PowerShell
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
  "$ErrorActionPreference = 'Stop'; " ^
  "$repo = '%REPO%'; " ^
  "$binDir = '%PLUGIN_ROOT%\bin'; " ^
  "try { " ^
  "  $release = Invoke-RestMethod -Uri \"https://api.github.com/repos/$repo/releases/latest\" -UseBasicParsing; " ^
  "  $version = $release.tag_name; " ^
  "  $target = 'x86_64-pc-windows-msvc'; " ^
  "  $url = \"https://github.com/$repo/releases/download/$version/claude-rlm-$target.zip\"; " ^
  "  $tmpDir = Join-Path $env:TEMP 'claude-rlm-bootstrap'; " ^
  "  New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null; " ^
  "  $zipPath = Join-Path $tmpDir 'claude-rlm.zip'; " ^
  "  Write-Host \"[claude-rlm] Downloading $version for $target...\" -NoNewline; " ^
  "  [System.Console]::Error.WriteLine(''); " ^
  "  Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing; " ^
  "  Expand-Archive -Path $zipPath -DestinationPath $tmpDir -Force; " ^
  "  New-Item -ItemType Directory -Path $binDir -Force | Out-Null; " ^
  "  Move-Item -Path (Join-Path $tmpDir 'claude-rlm.exe') -Destination (Join-Path $binDir 'claude-rlm.exe') -Force; " ^
  "  Set-Content -Path (Join-Path $binDir '.version') -Value $version; " ^
  "  Remove-Item -Path $tmpDir -Recurse -Force; " ^
  "  [System.Console]::Error.WriteLine(\"[claude-rlm] Installed claude-rlm $version\"); " ^
  "} catch { " ^
  "  [System.Console]::Error.WriteLine(\"[claude-rlm] ERROR: $_\"); " ^
  "  [System.Console]::Error.WriteLine(\"[claude-rlm] Download manually from https://github.com/$repo/releases\"); " ^
  "  exit 1; " ^
  "}"

exit /b %ERRORLEVEL%
