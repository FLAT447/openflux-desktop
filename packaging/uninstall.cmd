@echo off
rem OpenFlux Windows uninstaller: stops the engine, removes the install dir and the
rem PATH entry. Run from an elevated (Administrator) cmd.
setlocal
set "DEST=%ProgramFiles%\OpenFlux"

if exist "%DEST%\openflux.exe" (
    "%DEST%\openflux.exe" disconnect >nul 2>nul
    "%DEST%\openflux.exe" tun off >nul 2>nul
    "%DEST%\openflux.exe" exit off >nul 2>nul
)

rem Remove the PATH entry (machine-wide), then the directory.
set "NEWPATH="
for /f "delims=" %%p in ('echo %PATH%') do (
    echo %%p | findstr /I /C:"%DEST%" >nul || set "NEWPATH=!NEWPATH!;%%p"
)
if defined NEWPATH setx PATH "%NEWPATH:~1%" /M >nul

rmdir /s /q "%DEST%" 2>nul

echo OpenFlux removed from %ProgramFiles%. Wintun adapter will vanish once no session
echo keeps it open; reboot if the OpenFlux adapter lingers in Network Connections.
endlocal