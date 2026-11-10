@echo off
ping -n 15 127.0.0.1 > nul
echo done> "%~dp0prereq.done"
