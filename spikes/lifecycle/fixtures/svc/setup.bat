@echo off
rem S2/S3 extra: process spawned by services.exe (not a child of this cmd).
sc create vtestsvc binPath= "C:\windows\system32\cmd.exe /c %~dp0Redist\prereq.bat" type= own start= demand > "%~dp0svc.out" 2>&1
sc start vtestsvc >> "%~dp0svc.out" 2>&1
exit /b 0
