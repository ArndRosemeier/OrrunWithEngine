@echo off
setlocal
cd /d "%~dp0"
python -m tools.gamedata_viewer "%~dp0data\OrrunGameData.xml" %*
