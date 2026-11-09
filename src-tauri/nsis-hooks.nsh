; NSIS installer hooks for XPhoneUse (Tauri 2 custom hooks).
;
; The iOS stack talks to usbmuxd on 127.0.0.1:27015, provided on Windows by
; Apple's "Apple Mobile Device Support" component (also installs the usbaapl
; USB driver). Without it no iOS device is visible over USB, and the app
; cannot fix that at runtime — so the installer checks for it and, when
; missing, offers to fetch it and install it. The iTunes application itself
; is NOT installed: AppleMobileDeviceSupport64.msi ships inside the iTunes
; setup package and is all we need (Apple offers no standalone download).
;
; Two hard-won details of the Apple package (verified 2026-09-12):
;   - iTunes64Setup.exe /qn /norestart only installs iTunes64.msi — the
;     chained support packages are silently skipped, so AMDS must be
;     extracted (/extract <dir>) and installed explicitly.
;   - AMDS is a per-machine MSI (HKLM + Program Files). The XPhoneUse NSIS
;     bundle is currentUser mode and runs unelevated, so msiexec needs an
;     explicit elevation: PowerShell Start-Process -Verb RunAs -Wait, wrapped
;     in a .ps1 so the exit code can be checked. This costs one UAC prompt.
;   - Detection ground truth: 64-bit registry view "HKLM\SOFTWARE\Apple Inc.\
;     Apple Mobile Device Support" InstallDir points at ...
;     \Apple\Mobile Device Support\ containing AppleMobileDeviceService.exe.
;     NSIS is a 32-bit process, so the registry view must be switched
;     explicitly. Android support needs none of this.

; --- stopping the bundled adb before any file is written --------------------
;
; adb is a detached child of the app: the server that the first `adb devices`
; starts keeps running after the app is gone, with adb.exe and both AdbWinApi
; DLLs mapped out of $INSTDIR\resources. NSIS cannot overwrite a mapped file,
; so the install dies on "cannot write AdbWinApi.dll" (verified on a real
; install: the server was `D:\Local\XPhoneUse\resources\adb.exe`, holding the
; two DLLs from the very directory the installer rewrites). It looks like it
; comes out of nowhere because the app itself is closed by then.
;
; The app is closed *here*, before the adb kill, for one reason: it lists
; devices every 3 seconds, and any one of those polls would start a fresh
; server right after we stopped the old one. The installer's own app-running
; check closes it a moment later anyway (silently, during an update), so this
; only moves that step ahead of the kill it needs to survive.
;
; Only adb processes whose image lives inside $INSTDIR are stopped - an adb the
; user runs themselves (Android Studio, platform-tools on PATH) is left alone,
; which is also why this is not `taskkill /IM adb.exe`. Two passes half a
; second apart, because a poll already in flight can land in between.
!macro XPU_STOP_ADB
  Push $R0
  Push $R1

  nsis_tauri_utils::KillProcessCurrentUser "${MAINBINARYNAME}.exe"
  Pop $R0
  DetailPrint "adb: app closed (kill result $R0)"

  FileOpen $R0 "$TEMP\XPU-StopAdb.ps1" w
  FileWrite $R0 "$$dir = '$INSTDIR'$\r$\n"
  FileWrite $R0 "1..2 | ForEach-Object {$\r$\n"
  FileWrite $R0 "  Get-CimInstance Win32_Process -Filter $\"Name='adb.exe'$\" | Where-Object { $$_.ExecutablePath -and $$_.ExecutablePath.StartsWith($$dir, 'OrdinalIgnoreCase') } | ForEach-Object { Stop-Process -Id $$_.ProcessId -Force -ErrorAction SilentlyContinue }$\r$\n"
  FileWrite $R0 "  Start-Sleep -Milliseconds 500$\r$\n"
  FileWrite $R0 "}$\r$\n"
  FileClose $R0
  ExecWait 'powershell -NoProfile -ExecutionPolicy Bypass -File "$TEMP\XPU-StopAdb.ps1"' $R1
  Delete "$TEMP\XPU-StopAdb.ps1"
  DetailPrint "adb: bundled server stopped (powershell exit $R1)"

  Pop $R1
  Pop $R0
!macroend

!macro NSIS_HOOK_PREINSTALL
  Push $R0
  Push $R1
  Push $R2

  StrCpy $R0 ""
  SetRegView 64
  ReadRegStr $R0 HKLM "SOFTWARE\Apple Inc.\Apple Mobile Device Support" "InstallDir"
  SetRegView 32
  ${If} $R0 == ""
    ReadRegStr $R0 HKLM "SOFTWARE\Apple Inc.\Apple Mobile Device Support" "InstallDir"
  ${EndIf}
  ${IfThen} $R0 != "" ${|} IfFileExists "$R0\AppleMobileDeviceService.exe" amds_present 0 ${|}

  MessageBox MB_YESNO|MB_ICONQUESTION "未检测到 Apple 移动设备支持组件 — iOS USB 连接需要它。$\n$\n现在从 apple.com 下载（约 200 MB）并安装该组件吗？只装 usbmuxd 驱动组件，不会安装 iTunes 应用。$\n$\n选 否 跳过：iOS 功能将不可用，Android 不受影响。$\n$\nApple Mobile Device Support is required for iOS USB connections. Download (~200 MB from apple.com) and install it now? Only the usbmuxd component is installed, not the iTunes app.$\n$\nChoose No to skip — iOS features will be unavailable (Android unaffected)." IDYES amds_install
  DetailPrint "iOS support: Apple Mobile Device Support not present, skipped by user"
  Goto amds_done

amds_install:
  DetailPrint "iOS support: downloading Apple package..."
  NSISdl::download "https://www.apple.com/itunes/download/win64" "$TEMP\XPU-AppleSetup.exe"
  Pop $R1
  ${If} $R1 != "success"
    MessageBox MB_OK|MB_ICONEXCLAMATION "下载失败 ($R1)。请手动安装 iTunes 后重新运行安装程序：$\n$\nDownload failed. Install iTunes manually and re-run setup:$\nhttps://www.apple.com/itunes/download/"
    Goto amds_cleanup_exe
  ${EndIf}

  DetailPrint "iOS support: extracting Apple Mobile Device Support..."
  ; The target directory MUST exist first: with a non-existent path the
  ; bootstrapper returns 0 and extracts nothing (silent no-op).
  CreateDirectory "$TEMP\XPU-AppleCmp"
  ExecWait '"$TEMP\XPU-AppleSetup.exe" /extract "$TEMP\XPU-AppleCmp"' $R1
  ${If} $R1 != 0
    MessageBox MB_OK|MB_ICONEXCLAMATION "解包失败 (code $R1)。请手动安装 iTunes 后重新运行安装程序：$\n$\nExtraction failed (code $R1). Install iTunes manually and re-run setup:$\nhttps://www.apple.com/itunes/download/"
    Goto amds_cleanup
  ${EndIf}
  IfFileExists "$TEMP\XPU-AppleCmp\AppleMobileDeviceSupport64.msi" 0 amds_extract_bad

  DetailPrint "iOS support: installing Apple Mobile Device Support (needs UAC consent)..."
  FileOpen $R2 "$TEMP\XPU-InstallAm.ps1" w
  FileWrite $R2 'exit (Start-Process msiexec.exe -ArgumentList "/i","$TEMP\XPU-AppleCmp\AppleMobileDeviceSupport64.msi","/qn","/norestart" -Verb RunAs -Wait -PassThru).ExitCode'
  FileClose $R2
  ExecWait 'powershell -NoProfile -ExecutionPolicy Bypass -File "$TEMP\XPU-InstallAm.ps1"' $R1
  Delete "$TEMP\XPU-InstallAm.ps1"

  ; 0 = success, 3010 = success + reboot required, 1638 = newer version present
  ${If} $R1 == 0
  ${OrIf} $R1 == 3010
  ${OrIf} $R1 == 1638
    DetailPrint "iOS support: Apple Mobile Device Support installed (msiexec exit $R1)"
  ${Else}
    MessageBox MB_OK|MB_ICONEXCLAMATION "Apple 移动设备支持组件安装未成功 (code $R1)。iOS 功能可能不可用；可稍后手动安装 iTunes：$\n$\nApple Mobile Device Support install failed (code $R1); iOS features may be unavailable. It can be installed manually later via iTunes:$\nhttps://www.apple.com/itunes/download/"
    Goto amds_cleanup
  ${EndIf}

  ; Registry is the ground truth — trust it over the exit code alone.
  StrCpy $R0 ""
  SetRegView 64
  ReadRegStr $R0 HKLM "SOFTWARE\Apple Inc.\Apple Mobile Device Support" "InstallDir"
  SetRegView 32
  ${If} $R0 == ""
    ReadRegStr $R0 HKLM "SOFTWARE\Apple Inc.\Apple Mobile Device Support" "InstallDir"
  ${EndIf}
  ${If} $R0 == ""
    DetailPrint "iOS support: WARNING - install reported success but registry entry missing"
  ${EndIf}
  Goto amds_cleanup

amds_extract_bad:
  MessageBox MB_OK|MB_ICONEXCLAMATION "安装包内容不符合预期（未找到 AppleMobileDeviceSupport64.msi）。请手动安装 iTunes 后重新运行安装程序：$\n$\nUnexpected Apple package contents (AppleMobileDeviceSupport64.msi not found). Install iTunes manually and re-run setup:$\nhttps://www.apple.com/itunes/download/"

amds_cleanup:
  RMDir /r "$TEMP\XPU-AppleCmp"
amds_cleanup_exe:
  Delete "$TEMP\XPU-AppleSetup.exe"
  Goto amds_done

amds_present:
  DetailPrint "iOS support: Apple Mobile Device Support already present"

amds_done:
  ; Last thing before the files are written: nothing may hold them any more.
  ; After the Apple block on purpose - that one can wait on a download and a
  ; UAC prompt, and the app has no business being closed that long.
  !insertmacro XPU_STOP_ADB
  Pop $R2
  Pop $R1
  Pop $R0
!macroend

; The uninstaller deletes the same files, and hits the same lock.
!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro XPU_STOP_ADB
!macroend
