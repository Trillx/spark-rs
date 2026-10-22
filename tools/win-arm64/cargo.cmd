@echo off
rem Run cargo natively on Windows ARM64. Loads the MSVC ARM64 environment and
rem compiles C code with clang-cl, which aws-lc-sys requires on this target.
rem   tools\win-arm64\cargo.cmd test --workspace
rem   tools\win-arm64\cargo.cmd build --release -p spark-desktop -p spark-cli
setlocal

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath`) do set "VS=%%i"
if not defined VS (
  echo Visual Studio Build Tools with "MSVC ARM64 build tools" not found.
  exit /b 1
)
rem VsDevCmd.bat runs vswhere.exe by name.
set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;%PATH%"
call "%VS%\VC\Auxiliary\Build\vcvarsall.bat" arm64 >nul || exit /b 1

where clang-cl >nul 2>nul || set "PATH=%ProgramFiles%\LLVM\bin;%VS%\VC\Tools\Llvm\ARM64\bin;%PATH%"
where clang-cl >nul 2>nul || (
  echo clang-cl not found. Install LLVM ^(winget install LLVM.LLVM^) or the
  echo "C++ Clang Compiler for Windows" component of Visual Studio Build Tools.
  exit /b 1
)
where cargo >nul 2>nul || set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

set "CC_aarch64_pc_windows_msvc=clang-cl"
set "CXX_aarch64_pc_windows_msvc=clang-cl"
cd /d "%~dp0..\.."
cargo %*
