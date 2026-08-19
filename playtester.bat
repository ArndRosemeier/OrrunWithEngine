@echo off
setlocal
cd /d C:\Projekte\OrrunWithEngine
if not exist shots\playtester mkdir shots\playtester
if not exist shots\playtester\appdata mkdir shots\playtester\appdata
set ENGINE_SCREENSHOT=%CD%\shots\playtester
set ENGINE_SCREENSHOT_WAIT=1
set APPDATA=%CD%\shots\playtester\appdata
cargo run -p orrun --release --bin playtester -- --seed 1 --size 64 --hooks standing,dungeon_fill,bind