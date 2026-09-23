@echo off
rem OpenFlux Windows installer: copies the portable bundle onto the machine-wide
rem OpenFlux dir and adds it to PATH. Run from an elevated (Administrator) cmd.
setlocal
set "DEST=%ProgramFiles%\OpenFlux"
set "SRC=%~dp0"

if not exist "%SRC%openflux.exe" (
    echo openflux.exe not found next to this script. Run from an extracted bundle.
    exit /b 1
)

mkdir "%DEST%" 2>nul
copy /y "%SRC%openflux.exe" "%DEST%\openflux.exe" >nul
copy /y "%SRC%openflux-engine.exe" "%DEST%\openflux-engine.exe" >nul
if exist "%SRC%openflux-gui.exe" copy /y "%SRC%openflux-gui.exe" "%DEST%\openflux-gui.exe" >nul
if exist "%SRC%wintun.dll" copy /y "%SRC%wintun.dll" "%DEST%\wintun.dll" >nul

rem Machine-wide PATH entry (skipped if already present).
echo %PATH% | findstr /I /C:"%DEST%" >nul || setx PATH "%PATH%;%DEST%" /M >nul

echo OpenFlux installed to %DEST%. wintun.dll is bundled for TUN mode; run the GUI
echo (TUN button) or `openflux tun on` from an elevated terminal. TUN needs the
echo WireGuard Wintun driver installed (wintun.sys is loaded on demand by wintun.dll).
endlocal