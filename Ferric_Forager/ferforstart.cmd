@echo off
setlocal EnableExtensions DisableDelayedExpansion

set "REPO_ROOT=%~dp0"
set "START_AUTHORITY=%REPO_ROOT%START_HERE.yaml"
set "CODEX_AUTHORITY=%REPO_ROOT%.GOV\codex.yaml"
set "TOPOLOGY_AUTHORITY=%REPO_ROOT%.GOV\topology.yaml"

for %%F in ("%START_AUTHORITY%" "%CODEX_AUTHORITY%" "%TOPOLOGY_AUTHORITY%") do (
  if not exist "%%~fF" (
    >&2 echo [FERFORSTART-ERROR] Required authority file is missing: %%~fF
    exit /b 1
  )
)

echo [FERFORSTART-BEGIN]
echo Repository authority injection for Ferric Forager.
echo Repository root: %REPO_ROOT%
echo.
echo [FERFORSTART-INSTRUCTIONS]
echo Read every injected authority file completely before acting in this repository.
echo Treat the injected files as repository rules and instructions that must be followed.
echo Apply their stated authority precedence, navigation, task, safety, validation, and closure rules.
echo Do not claim acknowledgment until all three injected files have been read completely.
echo Stop and report the conflict if the injected authority is missing, contradictory, or cannot be followed.

call :emit_authority "%START_AUTHORITY%" "START_HERE.yaml"
if errorlevel 1 exit /b 1
call :emit_authority "%CODEX_AUTHORITY%" ".GOV/codex.yaml"
if errorlevel 1 exit /b 1
call :emit_authority "%TOPOLOGY_AUTHORITY%" ".GOV/topology.yaml"
if errorlevel 1 exit /b 1

echo.
echo [FERFORSTART-ACKNOWLEDGMENT-REQUIRED]
echo After reading all three files completely, acknowledge with this exact line:
echo FERRIC_FORAGER_AUTHORITY_ACK: Read START_HERE.yaml, .GOV/codex.yaml, and .GOV/topology.yaml completely; accepted them as repository rules and instructions; will follow them and their authority precedence.
echo [FERFORSTART-END]
exit /b 0

:emit_authority
echo.
echo ===== BEGIN AUTHORITY FILE: %~2 =====
type "%~1"
if errorlevel 1 (
  >&2 echo [FERFORSTART-ERROR] Failed to inject authority file: %~1
  exit /b 1
)
echo.
echo ===== END AUTHORITY FILE: %~2 =====
exit /b 0
