@echo off
rem Builds puttydeck.exe and its installer with the Visual Studio 2022 Build Tools.
setlocal
set "VS=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools"
call "%VS%\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
cd /d "%~dp0"
rem The C runtime is linked statically, so nothing else has to be on the target PC.
if not exist build-deck\build.ninja (
    cmake -G Ninja -S . -B build-deck -DCMAKE_BUILD_TYPE=Release -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded || exit /b 1
)
cmake --build build-deck --target puttydeck || exit /b 1
echo Built build-deck\puttydeck.exe

rc /nologo /fo build-deck\setup.res installer\setup.rc || exit /b 1
cl /nologo /O2 /MT /W3 /Fobuild-deck\setup.obj /Fe:build-deck\PuTTYDeck-0.85-setup.exe installer\setup.c build-deck\setup.res ^
    /link /SUBSYSTEM:WINDOWS user32.lib shell32.lib ole32.lib advapi32.lib propsys.lib uuid.lib || exit /b 1
echo Built build-deck\PuTTYDeck-0.85-setup.exe
