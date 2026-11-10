@echo off
rem S3 case 3a: start /b (same console, no new window) and exit at once.
start /b cmd /c Redist\prereq.bat
exit /b 0
