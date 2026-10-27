; Tauri defines MAINBINARYNAME only after including this file.
!define SPARK_APP_EXE "spark-desktop.exe"

; Ends every process running $INSTDIR\<name on the stack>, or a copy of it
; moved aside as <name>.N.old, and sets the error flag if one outlives a
; ten-second wait. Terminate the image directly: Restart Manager can signal
; the shared console and stop the terminal or agent that launched spark-cli,
; and it failed to close a running spark-rs ("Failed to kill spark-rs").
; NSIS is a 32-bit process on both architectures. PROCESSENTRY32W is 556
; bytes there, with the PID at byte 8 and the executable name at byte 36.
!macro SparkStopFunction prefix
Function ${prefix}SparkStop
  Exch $8
  Push $0
  Push $1
  Push $2
  Push $3
  Push $4
  Push $5
  Push $6
  Push $7
  StrCpy $1 0
  StrCpy $5 0
  StrCpy $7 0
  System::Call 'kernel32::CreateToolhelp32Snapshot(i 2, i 0) p .r0'
  StrCmp $0 -1 failed
  System::Alloc 556
  Pop $1
  StrCmp $1 0 failed
  System::Call '*$1(i 556)'
  System::Call 'kernel32::Process32FirstW(p r0, p r1) i .r2'
  process_loop:
    StrCmp $2 0 finished
    IntOp $3 $1 + 36
    System::Call '*$3(&w260 .r4)'
    StrCmp $4 $8 0 next_process
    IntOp $3 $1 + 8
    System::Call '*$3(i .r4)'
    System::Call 'kernel32::OpenProcess(i 0x101001, i 0, i r4) p .r5'
    StrCmp $5 0 next_process
    System::Call 'kernel32::QueryFullProcessImageNameW(p r5, i 0, w .r6, *i ${NSIS_MAX_STRLEN}) i .r2'
    StrCmp $2 0 close_process
    ; The prefix also matches copies that updates moved aside.
    StrLen $3 '$INSTDIR\$8'
    StrCpy $4 $6 $3
    StrCmp $4 '$INSTDIR\$8' 0 close_process
    ; Termination fails if it quit since the snapshot; the wait says so.
    System::Call 'kernel32::TerminateProcess(p r5, i 0)'
    System::Call 'kernel32::WaitForSingleObject(p r5, i 10000) i .r2'
    StrCmp $2 0 close_process
    StrCpy $7 1
  close_process:
    System::Call 'kernel32::CloseHandle(p r5)'
    StrCpy $5 0
  next_process:
    System::Call 'kernel32::Process32NextW(p r0, p r1) i .r2'
    Goto process_loop
  failed:
    StrCpy $7 1
  finished:
    ${If} $5 != 0
      System::Call 'kernel32::CloseHandle(p r5)'
    ${EndIf}
    ${If} $1 != 0
      System::Free $1
    ${EndIf}
    ${If} $0 != -1
      System::Call 'kernel32::CloseHandle(p r0)'
    ${EndIf}
    ${If} $7 = 0
      ClearErrors
    ${Else}
      SetErrors
    ${EndIf}
  Pop $7
  Pop $6
  Pop $5
  Pop $4
  Pop $3
  Pop $2
  Pop $1
  Pop $0
  Pop $8
FunctionEnd
!macroend
!insertmacro SparkStopFunction ""
!insertmacro SparkStopFunction "un."

!macro SparkStopCli prefix
  Push spark-cli.exe
  Call ${prefix}SparkStop
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "Couldn't stop the installed spark-cli. Close agents using spark-rs and try again."
    SetErrorLevel 1
    Abort
  ${EndIf}
!macroend

; While this file is new, spark-cli waits for the new app instead of
; starting the old one (UPDATING_FILE in crates/agent/src/lib.rs).
Var SparkUpdating
; Where this install moved the installed spark-cli and app, if it did.
Var SparkCliAside
Var SparkAppAside

Function SparkMarkUpdating
  Push $0
  Delete $SparkUpdating
  ClearErrors
  FileOpen $0 $SparkUpdating w
  ${IfNot} ${Errors}
    FileClose $0
  ${EndIf}
  ClearErrors
  Pop $0
FunctionEnd

; Windows won't overwrite a running program, but it will rename one. Renames
; $INSTDIR\<name on the stack> to <name>.N.old and returns the new path, or
; "" if there was nothing to rename or it failed. First deletes the copies
; earlier updates moved aside, once nothing runs them.
Function SparkMoveAside
  Exch $1
  Push $0
  Delete "$INSTDIR\$1.*.old"
  StrCpy $0 0
  ${If} ${FileExists} "$INSTDIR\$1"
    ${Do}
      IntOp $0 $0 + 1
    ${LoopWhile} ${FileExists} "$INSTDIR\$1.$0.old"
    ClearErrors
    Rename "$INSTDIR\$1" "$INSTDIR\$1.$0.old"
    ${If} ${Errors}
      StrCpy $0 0
    ${EndIf}
  ${EndIf}
  ${If} $0 = 0
    StrCpy $1 ""
  ${Else}
    StrCpy $1 "$INSTDIR\$1.$0.old"
  ${EndIf}
  ClearErrors
  Pop $0
  Exch $1
FunctionEnd

; A failed install leaves the old copy working and lets agents start it.
Function .onInstFailed
  ${If} $SparkCliAside != ""
  ${AndIfNot} ${FileExists} "$INSTDIR\spark-cli.exe"
    Rename $SparkCliAside "$INSTDIR\spark-cli.exe"
  ${EndIf}
  ${If} $SparkAppAside != ""
  ${AndIfNot} ${FileExists} "$INSTDIR\${SPARK_APP_EXE}"
    Rename $SparkAppAside "$INSTDIR\${SPARK_APP_EXE}"
  ${EndIf}
  ${If} $SparkUpdating != ""
    Delete $SparkUpdating
  ${EndIf}
FunctionEnd

; The PRE hooks run before replacing or removing any installed files.
!macro NSIS_HOOK_PREINSTALL
  !if "${ARCH}" == "arm64"
    ${IfNot} ${IsNativeARM64}
      MessageBox MB_OK|MB_ICONSTOP "This installer requires Windows on ARM. Download the Windows x64 installer from github.com/Trillx/spark-rs/releases/latest."
      SetErrorLevel 1
      Abort
    ${EndIf}
  !else if "${ARCH}" == "x64"
    ${If} ${IsNativeARM64}
      IfSilent spark_cpu_done
      MessageBox MB_OKCANCEL|MB_ICONINFORMATION "This is the x64 installer. The ARM64 installer runs faster on this computer. Continue with x64?" IDOK spark_cpu_done
      SetErrorLevel 1
      Abort
    ${EndIf}
  !endif
  spark_cpu_done:
  CreateDirectory "$LOCALAPPDATA\${BUNDLEID}"
  StrCpy $SparkUpdating "$LOCALAPPDATA\${BUNDLEID}\updating"
  Call SparkMarkUpdating
  ; Agents keep spark-cli running for as long as they run. Their copy
  ; carries on under another name and the new spark-cli takes its place.
  Push spark-cli.exe
  Call SparkMoveAside
  Pop $SparkCliAside
  ; Something kept it from being renamed: end its processes instead.
  ${If} ${FileExists} "$INSTDIR\spark-cli.exe"
    !insertmacro SparkStopCli ""
  ${EndIf}
  ; Moved aside first, so nothing can start the old app again, then ended
  ; here because Tauri's check after this hook couldn't close it. If it
  ; won't end, its renamed file still doesn't hold up the install.
  Push ${SPARK_APP_EXE}
  Call SparkMoveAside
  Pop $SparkAppAside
  Push ${SPARK_APP_EXE}
  Call SparkStop
  ClearErrors
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Gone now unless something still runs them; a later update tries again.
  ${If} $SparkCliAside != ""
    Delete $SparkCliAside
  ${EndIf}
  ${If} $SparkAppAside != ""
    Delete $SparkAppAside
  ${EndIf}
  Delete $SparkUpdating
  ClearErrors
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro SparkStopCli "un."
  Delete "$INSTDIR\spark-cli.exe.*.old"
  ; Before Tauri's own check, which couldn't close it.
  Push ${SPARK_APP_EXE}
  Call un.SparkStop
  ClearErrors
  Delete "$INSTDIR\${SPARK_APP_EXE}.*.old"
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    !insertmacro CheckIfAppIsRunning "$INSTDIR\${MAINBINARYNAME}.exe" "spark-rs"
    ClearErrors
    ExecWait '"$INSTDIR\${MAINBINARYNAME}.exe" --forget-saved-passwords' $0
    ${If} ${Errors}
    ${OrIf} $0 != 0
      MessageBox MB_OK|MB_ICONSTOP "Couldn't delete saved passwords. Close spark-rs and try uninstalling again."
      SetErrorLevel 1
      Abort
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $UpdateMode <> 1
    DeleteRegKey HKCU "Software\Classes\AppUserModelId\${BUNDLEID}"
  ${EndIf}
!macroend
