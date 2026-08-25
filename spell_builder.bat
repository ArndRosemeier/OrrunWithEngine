@echo off
setlocal
cd /d "%~dp0"
python -m tools.spell_builder ui %*