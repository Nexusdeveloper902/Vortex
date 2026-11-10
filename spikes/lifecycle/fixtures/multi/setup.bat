@echo off
rem S4/S5: reads data1.bin by RELATIVE path; records what Windows sees as cwd.
echo cwd=%CD%> "%~dp0result.txt"
echo dp0=%~dp0>> "%~dp0result.txt"
echo marker=%VORTEX_RUN_ID%>> "%~dp0result.txt"
if exist data1.bin (echo data1_rel=found>> "%~dp0result.txt") else (echo data1_rel=missing>> "%~dp0result.txt")
type data1.bin >> "%~dp0result.txt" 2>&1
if exist setup-1.bin (echo setup1_rel=found>> "%~dp0result.txt") else (echo setup1_rel=missing>> "%~dp0result.txt")
call Redist\prereq.bat
