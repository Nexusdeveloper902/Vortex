@echo off
ping -n 10 127.0.0.1 > nul
echo done> "%~dp0stage2.done"
