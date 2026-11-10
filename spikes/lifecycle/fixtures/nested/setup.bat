@echo off
rem S3 case 2: start a separate cmd for the prereq and exit at once.
start "" cmd /c Redist\prereq.bat
exit /b 0
