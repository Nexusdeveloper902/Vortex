@echo off
rem S2/S3 extra: process created through WMI Win32_Process.Create instead of a child cmd.
wmic process call create "cmd /c %~dp0Redist\prereq.bat" > "%~dp0wmic.out" 2>&1
exit /b 0
