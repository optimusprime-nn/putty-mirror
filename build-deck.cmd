@echo off
rem Builds puttydeck.exe with the Visual Studio 2022 Build Tools.
setlocal
set "VS=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
call "%VS%\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
cd /d "%~dp0"
if not exist build-deck\build.ninja (
    cmake -G Ninja -S . -B build-deck -DCMAKE_BUILD_TYPE=Release || exit /b 1
)
cmake --build build-deck --target puttydeck || exit /b 1
echo Built build-deck\puttydeck.exe
