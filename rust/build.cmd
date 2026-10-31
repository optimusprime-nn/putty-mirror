@echo off
rem Builds rustutty.exe and its installer into dist. Needs Rust and the Visual
rem Studio 2022 Build Tools (C++), which provide the linker and resource compiler.
setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
cd /d "%~dp0"
cargo build --release -p rustutty -p deck-tools || exit /b 1
set "DECK_EXE=%CD%\target\release\rustutty.exe"
set "DECK_TOOLS=%CD%\target\release"
cargo build --release -p deck-setup || exit /b 1
if not exist dist mkdir dist
copy /y target\release\rustutty.exe dist\ >nul || (
    echo Could not update dist\rustutty.exe. Is RustuTTY still running?
    exit /b 1
)
for %%t in (plink pscp psftp puttygen pageant) do copy /y target\release\%%t.exe dist\ >nul || exit /b 1
copy /y target\release\setup.exe dist\RustuTTY-1.0.0-setup.exe >nul || exit /b 1
echo Built dist\rustutty.exe and dist\RustuTTY-1.0.0-setup.exe
