; NSIS is a 32-bit process on both architectures. PROCESSENTRY32W is 556
; bytes there, with the PID at byte 8 and the executable name at byte 36.
; Terminate the matching image directly: Restart Manager can signal the
; shared console and stop the terminal or agent that launched spark-cli.
!macro SparkStopCliFunction prefix
Function ${prefix}SparkStopCli
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
    StrCmp $4 'spark-cli.exe' 0 next_process
    IntOp $3 $1 + 8
    System::Call '*$3(i .r4)'
    System::Call 'kernel32::OpenProcess(i 0x101001, i 0, i r4) p .r5'
    StrCmp $5 0 next_process
    System::Call 'kernel32::QueryFullProcessImageNameW(p r5, i 0, w .r6, *i ${NSIS_MAX_STRLEN}) i .r2'
    StrCmp $2 0 close_process
    ; The prefix also matches copies that updates moved aside.
    StrLen $3 '$INSTDIR\spark-cli.exe'
    StrCpy $4 $6 $3
    StrCmp $4 '$INSTDIR\spark-cli.exe' 0 close_process
    ; Termination fails if it quit since the snapshot; the wait says so.
    System::Call 'kernel32::TerminateProcess(p r5, i 0)'
    System::Call 'kernel32::WaitForSingleObject(p r5, i 10000) i .r2'
    StrCmp $2 0 close_process failed
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
FunctionEnd
!macroend
!insertmacro SparkStopCliFunction ""
!insertmacro SparkStopCliFunction "un."

!macro SparkStopCli prefix
  Call ${prefix}SparkStopCli
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "Couldn't stop the installed spark-cli. Close agents using spark-rs and try again."
    SetErrorLevel 1
    Abort
  ${EndIf}
!macroend

; While this file is new, spark-cli waits for the new app instead of
; starting the old one (UPDATING_FILE in crates/agent/src/lib.rs).
Var SparkUpdating
; Where this install moved the installed spark-cli, if it did.
Var SparkCliAside

; Agents keep spark-cli running for as long as they run, and Windows won't
; overwrite a running program, but it will rename one: the agents' copy
; carries on under another name and the new spark-cli takes its place.
Function SparkBeginUpdate
  Push $0
  Delete $SparkUpdating
  ClearErrors
  FileOpen $0 $SparkUpdating w
  ${IfNot} ${Errors}
    FileClose $0
  ${EndIf}
  StrCpy $SparkCliAside ""
  ; Copies moved aside by earlier updates, once their agents have quit.
  Delete "$INSTDIR\spark-cli.exe.*.old"
  ${If} ${FileExists} "$INSTDIR\spark-cli.exe"
    StrCpy $0 0
    ${Do}
      IntOp $0 $0 + 1
    ${LoopWhile} ${FileExists} "$INSTDIR\spark-cli.exe.$0.old"
    ClearErrors
    Rename "$INSTDIR\spark-cli.exe" "$INSTDIR\spark-cli.exe.$0.old"
    ${IfNot} ${Errors}
      StrCpy $SparkCliAside "$INSTDIR\spark-cli.exe.$0.old"
    ${EndIf}
  ${EndIf}
  ClearErrors
  Pop $0
FunctionEnd

; A failed install leaves the old copy working, with its own spark-cli, and
; lets agents start it again.
Function .onInstFailed
  ${If} $SparkCliAside != ""
  ${AndIfNot} ${FileExists} "$INSTDIR\spark-cli.exe"
    Rename $SparkCliAside "$INSTDIR\spark-cli.exe"
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
  Call SparkBeginUpdate
  ; Something kept it from being renamed: end its processes instead.
  ${If} ${FileExists} "$INSTDIR\spark-cli.exe"
    !insertmacro SparkStopCli ""
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; Gone now unless an agent still runs it; a later update tries again.
  ${If} $SparkCliAside != ""
    Delete $SparkCliAside
  ${EndIf}
  Delete $SparkUpdating
  ClearErrors
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro SparkStopCli "un."
  Delete "$INSTDIR\spark-cli.exe.*.old"
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
