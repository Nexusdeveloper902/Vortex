@echo off
rem S3 case 3b: stage1 starts stage2 only after setup.bat has exited.
start "" cmd /c stage1.bat
exit /b 0
