@echo off
setlocal
cd /d "%~dp0"
cargo run -p orrun --bin hamlet_lab -- %*
