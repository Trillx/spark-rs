; The hooks run before replacing or removing any installed files. Restart
; Manager identifies the exact path, so other copies' agents are unaffected.
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
  !insertmacro CheckIfAppIsRunning "$INSTDIR\spark-cli.exe" "Agents using spark-rs"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro CheckIfAppIsRunning "$INSTDIR\spark-cli.exe" "Agents using spark-rs"
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
