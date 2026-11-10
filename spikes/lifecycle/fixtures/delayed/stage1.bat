@echo off
ping -n 5 127.0.0.1 > nul
start /b cmd /c stage2.bat
exit /b 0
