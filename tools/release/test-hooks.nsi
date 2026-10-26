; Runs the production CLI stop function without rebuilding the Rust app.
Unicode true
RequestExecutionLevel user
SilentInstall silent
!include LogicLib.nsh
!include "../../apps/desktop/src-tauri/installer/hooks.nsh"
OutFile "stop-installed-cli.exe"
Section
  Call SparkStopCli
  IfErrors failed passed
  failed:
    SetErrorLevel 1
    Quit
  passed:
    SetErrorLevel 0
SectionEnd
