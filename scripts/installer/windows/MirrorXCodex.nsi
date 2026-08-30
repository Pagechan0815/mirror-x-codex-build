Unicode true
!include "MUI2.nsh"

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!define ROOT "..\..\.."
!define WEBVIEW2_BOOTSTRAPPER_URL "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
!define WEBVIEW2_DOWNLOAD_PAGE_URL "https://developer.microsoft.com/microsoft-edge/webview2#download-the-webview2-runtime"
!define WEBVIEW2_RUNTIME_CLIENT_ID "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!ifndef OUTPUT_FILE
  !define OUTPUT_FILE "${ROOT}\dist\windows\mirror-x-codex-${VERSION}-windows-x64-setup.exe"
!endif

Name "mirror x codex"
OutFile "${OUTPUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\Mirror X Codex"
InstallDirRegKey HKCU "Software\MirrorXCodex" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma
ShowInstDetails show
ShowUninstDetails show

!define MUI_ICON "${ROOT}\apps\codex-plus-manager\src-tauri\icons\icon.ico"
!define MUI_UNICON "${ROOT}\apps\codex-plus-manager\src-tauri\icons\icon.ico"
!define MUI_FINISHPAGE_RUN "$INSTDIR\mirror-x-codex-manager.exe"
!define MUI_FINISHPAGE_RUN_TEXT "启动 mirror x codex"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Var StageDir
Var BackupDir

!macro DefineStopProductProcess PREFIX
Function ${PREFIX}StopProductProcess
  Exch $0
  Push $1
  Push $2
  StrCpy $2 0
  nsExec::ExecToLog '"$SYSDIR\taskkill.exe" /IM "$0"'
  Pop $1
  StrCmp $1 "0" process_stopped
  StrCmp $1 "128" process_stop_confirmed
  Goto process_stop_failed
process_stopped:
  Sleep 500
  IntOp $2 $2 + 1
  nsExec::ExecToLog '"$SYSDIR\taskkill.exe" /IM "$0"'
  Pop $1
  StrCmp $1 "128" process_stop_confirmed
  StrCmp $1 "0" 0 process_stop_failed
  IntCmp $2 10 process_stop_failed process_stopped process_stop_failed
process_stop_failed:
  SetErrorLevel 2
  IfSilent process_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "无法确认 $0 已完全退出（taskkill 返回 $1）。请先退出相关程序后重试；尚未覆盖或删除任何程序文件。"
process_abort_silent:
  Abort
process_stop_confirmed:
  Pop $2
  Pop $1
  Pop $0
FunctionEnd
!macroend

!insertmacro DefineStopProductProcess ""
!insertmacro DefineStopProductProcess "un."

Function CleanupStage
  Delete "$StageDir\mirror-x-codex.exe"
  Delete "$StageDir\mirror-x-codex-manager.exe"
  Delete "$StageDir\mirror-x-imagegen.exe"
  Delete "$StageDir\uninstall.exe"
  RMDir "$StageDir"
  ClearErrors
FunctionEnd

Function CleanupBackup
  ClearErrors
  Delete "$BackupDir\transaction.pending"
  IfFileExists "$BackupDir\transaction.pending" cleanup_backup_failed 0
  Delete "$BackupDir\transaction.backing-up"
  IfFileExists "$BackupDir\transaction.backing-up" cleanup_backup_failed 0
  Delete "$BackupDir\mirror-x-codex.exe"
  Delete "$BackupDir\mirror-x-codex-manager.exe"
  Delete "$BackupDir\mirror-x-imagegen.exe"
  Delete "$BackupDir\uninstall.exe"
  RMDir "$BackupDir"
  IfFileExists "$BackupDir\*.*" cleanup_backup_failed cleanup_backup_done
cleanup_backup_failed:
  SetErrors
  Return
cleanup_backup_done:
  ClearErrors
FunctionEnd

Function RollbackUpgrade
  DetailPrint "正在恢复升级前的程序文件..."
  ClearErrors

  IfFileExists "$BackupDir\mirror-x-codex.exe" restore_launcher 0
  IfFileExists "$StageDir\mirror-x-codex.exe" rollback_manager 0
  ClearErrors
  Delete "$INSTDIR\mirror-x-codex.exe"
  IfErrors rollback_failed
  Goto rollback_manager
restore_launcher:
  ClearErrors
  Delete "$INSTDIR\mirror-x-codex.exe"
  CopyFiles /SILENT "$BackupDir\mirror-x-codex.exe" "$INSTDIR"
  IfErrors rollback_failed
rollback_manager:
  IfFileExists "$BackupDir\mirror-x-codex-manager.exe" restore_manager 0
  IfFileExists "$StageDir\mirror-x-codex-manager.exe" rollback_imagegen 0
  ClearErrors
  Delete "$INSTDIR\mirror-x-codex-manager.exe"
  IfErrors rollback_failed
  Goto rollback_imagegen
restore_manager:
  ClearErrors
  Delete "$INSTDIR\mirror-x-codex-manager.exe"
  CopyFiles /SILENT "$BackupDir\mirror-x-codex-manager.exe" "$INSTDIR"
  IfErrors rollback_failed
rollback_imagegen:
  IfFileExists "$BackupDir\mirror-x-imagegen.exe" restore_imagegen 0
  IfFileExists "$StageDir\mirror-x-imagegen.exe" rollback_uninstaller 0
  ClearErrors
  Delete "$INSTDIR\mirror-x-imagegen.exe"
  IfErrors rollback_failed
  Goto rollback_uninstaller
restore_imagegen:
  ClearErrors
  Delete "$INSTDIR\mirror-x-imagegen.exe"
  CopyFiles /SILENT "$BackupDir\mirror-x-imagegen.exe" "$INSTDIR"
  IfErrors rollback_failed
rollback_uninstaller:
  IfFileExists "$BackupDir\uninstall.exe" restore_uninstaller 0
  IfFileExists "$StageDir\uninstall.exe" restore_finished 0
  ClearErrors
  Delete "$INSTDIR\uninstall.exe"
  IfErrors rollback_failed
  Goto restore_finished
restore_uninstaller:
  ClearErrors
  Delete "$INSTDIR\uninstall.exe"
  CopyFiles /SILENT "$BackupDir\uninstall.exe" "$INSTDIR"
  IfErrors rollback_failed
restore_finished:
  IfErrors rollback_failed
  Call CleanupStage
  Call CleanupBackup
  IfErrors rollback_failed
  ClearErrors
  Return
rollback_failed:
  SetErrors
FunctionEnd

Function .onInit
  ReadRegStr $0 HKCU "Software\MirrorXCodex" "InstallDir"
  StrCmp $0 "" 0 init_done
  ReadRegStr $0 HKCU "Software\MirrorPlus" "InstallDir"
  StrCmp $0 "" init_done
  StrCpy $INSTDIR $0
init_done:
FunctionEnd

Function DetectWebView2
  Push $0
  StrCpy $0 ""
  SetRegView 32
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_RUNTIME_CLIENT_ID}" "pv"
  StrCmp $0 "" webview_check_current_user 0
  StrCmp $0 "0.0.0.0" webview_check_current_user webview_detected

webview_check_current_user:
  ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\${WEBVIEW2_RUNTIME_CLIENT_ID}" "pv"
  StrCmp $0 "" webview_not_detected 0
  StrCmp $0 "0.0.0.0" webview_not_detected webview_detected

webview_not_detected:
  StrCpy $0 "0"
  Goto webview_detection_done
webview_detected:
  StrCpy $0 "1"
webview_detection_done:
  SetRegView 32
  Exch $0
FunctionEnd

Function EnsureWebView2
  Call DetectWebView2
  Pop $0
  StrCmp $0 "1" webview_ready

  InitPluginsDir
  StrCpy $0 "$PLUGINSDIR\MicrosoftEdgeWebview2Setup.exe"
webview_download_retry:
  Delete "$0"
  DetailPrint "未检测到 Microsoft Edge WebView2 Runtime，正在从 Microsoft 获取官方安装程序..."
  StrCpy $2 "下载工具不可用"

  IfFileExists "$SYSDIR\curl.exe" 0 webview_download_powershell
  nsExec::ExecToLog '"$SYSDIR\curl.exe" --fail --location --silent --show-error --connect-timeout 15 --max-time 120 --output "$0" "${WEBVIEW2_BOOTSTRAPPER_URL}"'
  Pop $1
  StrCmp $1 "0" webview_verify_signature
  StrCpy $2 "curl 返回 $1"

webview_download_powershell:
  IfFileExists "$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" 0 webview_download_failed
  nsExec::ExecToLog `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$ProgressPreference = 'SilentlyContinue'; try { Invoke-WebRequest -UseBasicParsing -Uri '${WEBVIEW2_BOOTSTRAPPER_URL}' -OutFile '$0' -TimeoutSec 120 -ErrorAction Stop; exit 0 } catch { exit 1 }"`
  Pop $1
  StrCmp $1 "0" webview_verify_signature
  StrCpy $2 "$2；PowerShell 返回 $1"

webview_download_failed:
  Delete "$0"
  SetErrorLevel 3
  IfSilent webview_abort_silent 0
  MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "Mirror X Codex 需要 Microsoft Edge WebView2 Runtime。Microsoft 官方安装程序下载失败：$2。$\r$\n$\r$\n点击“重试”再次下载；点击“取消”会打开 Microsoft 官方下载页并安全退出。旧版本尚未被覆盖。" IDRETRY webview_download_retry IDCANCEL webview_open_download_page
webview_abort_silent:
  Abort

webview_open_download_page:
  ExecShell "open" "${WEBVIEW2_DOWNLOAD_PAGE_URL}"
  Abort

webview_verify_signature:
  IfFileExists "$0" 0 webview_signature_failed
  IfFileExists "$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" 0 webview_signature_failed
  nsExec::ExecToLog `"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "$$signature = Get-AuthenticodeSignature -LiteralPath '$0'; if ($$signature.Status -eq 'Valid' -and $$signature.SignerCertificate.Subject.Split(',')[0] -eq 'CN=Microsoft Corporation') { exit 0 }; exit 1"`
  Pop $1
  StrCmp $1 "0" webview_downloaded

webview_signature_failed:
  StrCpy $2 "下载文件未通过 Microsoft 数字签名校验"
  Goto webview_download_failed

webview_downloaded:
  DetailPrint "正在安装 Microsoft Edge WebView2 Runtime..."
  ExecWait '"$0" /silent /install' $1
  Delete "$0"
  StrCmp $1 "3010" 0 webview_check_registration
  SetErrorLevel 3
  IfSilent webview_install_abort_silent 0
  Goto webview_restart_required

webview_check_registration:
  Call DetectWebView2
  Pop $2
  StrCmp $2 "1" webview_ready
  SetErrorLevel 3
  IfSilent webview_install_abort_silent 0
  MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "Microsoft Edge WebView2 Runtime 安装后仍未通过官方注册表检测（返回 $1）。$\r$\n$\r$\n点击“重试”重新下载并安装；点击“取消”会打开 Microsoft 官方下载页并安全退出。旧版本尚未被覆盖。" IDRETRY webview_download_retry IDCANCEL webview_open_download_page
webview_install_abort_silent:
  Abort

webview_restart_required:
  MessageBox MB_OK|MB_ICONINFORMATION "Microsoft Edge WebView2 Runtime 需要重启 Windows 后完成注册。旧版本尚未被覆盖；请重启后重新运行本安装包。"
  Abort

webview_ready:
  SetErrorLevel 0
  ClearErrors
FunctionEnd

Section "Install"
  Call EnsureWebView2
  SetOutPath "$INSTDIR"
  StrCpy $StageDir "$INSTDIR\.mirror-x-update-stage"
  StrCpy $BackupDir "$INSTDIR\.mirror-x-update-backup"

  IfFileExists "$INSTDIR\mirror-x-codex.exe" 0 stop_current_manager
  Push "mirror-x-codex.exe"
  Call StopProductProcess
stop_current_manager:
  IfFileExists "$INSTDIR\mirror-x-codex-manager.exe" 0 stop_current_imagegen
  Push "mirror-x-codex-manager.exe"
  Call StopProductProcess
stop_current_imagegen:
  IfFileExists "$INSTDIR\mirror-x-imagegen.exe" 0 stop_legacy_launcher
  Push "mirror-x-imagegen.exe"
  Call StopProductProcess
stop_legacy_launcher:
  IfFileExists "$INSTDIR\codex-plus-plus.exe" 0 stop_legacy_manager
  Push "codex-plus-plus.exe"
  Call StopProductProcess
stop_legacy_manager:
  IfFileExists "$INSTDIR\codex-plus-plus-manager.exe" 0 product_processes_stopped
  Push "codex-plus-plus-manager.exe"
  Call StopProductProcess
product_processes_stopped:

  IfFileExists "$BackupDir\transaction.pending" 0 no_interrupted_upgrade
  DetailPrint "检测到上次未完成的升级，先恢复旧版本。"
  Call RollbackUpgrade
  IfErrors recovery_failed
no_interrupted_upgrade:
  Call CleanupStage
  CreateDirectory "$StageDir"
  SetOutPath "$StageDir"
  ClearErrors
  File /oname=mirror-x-codex.exe "${ROOT}\dist\windows\app\mirror-x-codex.exe"
  File /oname=mirror-x-codex-manager.exe "${ROOT}\dist\windows\app\mirror-x-codex-manager.exe"
  File /oname=mirror-x-imagegen.exe "${ROOT}\dist\windows\app\mirror-x-imagegen.exe"
  WriteUninstaller "$StageDir\uninstall.exe"
  IfErrors staging_failed

  Call CleanupBackup
  IfErrors backup_cleanup_failed
  CreateDirectory "$BackupDir"
  ClearErrors
  FileOpen $0 "$BackupDir\transaction.backing-up" w
  IfErrors backup_failed
  FileWrite $0 "mirror x codex upgrade ${VERSION}$\r$\n"
  IfErrors backup_marker_write_failed
  FileClose $0
  IfErrors backup_failed
  Goto backup_marker_ready
backup_marker_write_failed:
  FileClose $0
  Goto backup_failed
backup_marker_ready:

  IfFileExists "$INSTDIR\mirror-x-codex.exe" 0 backup_manager
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\mirror-x-codex.exe" "$BackupDir"
  IfErrors backup_failed
backup_manager:
  IfFileExists "$INSTDIR\mirror-x-codex-manager.exe" 0 backup_imagegen
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\mirror-x-codex-manager.exe" "$BackupDir"
  IfErrors backup_failed
backup_imagegen:
  IfFileExists "$INSTDIR\mirror-x-imagegen.exe" 0 backup_uninstaller
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\mirror-x-imagegen.exe" "$BackupDir"
  IfErrors backup_failed
backup_uninstaller:
  IfFileExists "$INSTDIR\uninstall.exe" 0 commit_backup_manifest
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\uninstall.exe" "$BackupDir"
  IfErrors backup_failed

commit_backup_manifest:
  ClearErrors
  Rename "$BackupDir\transaction.backing-up" "$BackupDir\transaction.pending"
  IfErrors backup_failed
  IfFileExists "$BackupDir\transaction.pending" install_new_files backup_failed

install_new_files:
  ClearErrors
  Delete "$INSTDIR\mirror-x-codex.exe"
  IfErrors install_rollback
  Rename "$StageDir\mirror-x-codex.exe" "$INSTDIR\mirror-x-codex.exe"
  IfErrors install_rollback
  Delete "$INSTDIR\mirror-x-codex-manager.exe"
  IfErrors install_rollback
  Rename "$StageDir\mirror-x-codex-manager.exe" "$INSTDIR\mirror-x-codex-manager.exe"
  IfErrors install_rollback
  Delete "$INSTDIR\mirror-x-imagegen.exe"
  IfErrors install_rollback
  Rename "$StageDir\mirror-x-imagegen.exe" "$INSTDIR\mirror-x-imagegen.exe"
  IfErrors install_rollback
  Delete "$INSTDIR\uninstall.exe"
  IfErrors install_rollback
  Rename "$StageDir\uninstall.exe" "$INSTDIR\uninstall.exe"
  IfErrors install_rollback

  SetOutPath "$INSTDIR"
  ClearErrors
  CreateShortcut "$DESKTOP\mirror x codex.lnk" "$INSTDIR\mirror-x-codex.exe" "" "$INSTDIR\mirror-x-codex.exe"
  CreateShortcut "$DESKTOP\mirror x codex 管理器.lnk" "$INSTDIR\mirror-x-codex-manager.exe" "" "$INSTDIR\mirror-x-codex-manager.exe"
  CreateDirectory "$SMPROGRAMS\mirror x codex"
  CreateShortcut "$SMPROGRAMS\mirror x codex\mirror x codex.lnk" "$INSTDIR\mirror-x-codex.exe" "" "$INSTDIR\mirror-x-codex.exe"
  CreateShortcut "$SMPROGRAMS\mirror x codex\mirror x codex 管理器.lnk" "$INSTDIR\mirror-x-codex-manager.exe" "" "$INSTDIR\mirror-x-codex-manager.exe"
  CreateShortcut "$SMPROGRAMS\mirror x codex\卸载 mirror x codex.lnk" "$INSTDIR\uninstall.exe" "" "$INSTDIR\mirror-x-codex-manager.exe"

  WriteRegStr HKCU "Software\MirrorXCodex" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "DisplayName" "mirror x codex"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "Publisher" "镜子AI"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "DisplayIcon" "$INSTDIR\mirror-x-codex-manager.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "UninstallString" '$\"$INSTDIR\uninstall.exe$\"'
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "QuietUninstallString" '$\"$INSTDIR\uninstall.exe$\" /S'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex" "NoRepair" 1
  IfErrors install_rollback

  Call CleanupStage
  Call CleanupBackup
  IfErrors backup_cleanup_after_install_failed
  Goto cleanup_legacy_install

backup_cleanup_after_install_failed:
  IfFileExists "$BackupDir\transaction.pending" install_rollback 0
  DetailPrint "新版本已安装，但旧版本备份仍被占用；下次升级会先重新清理。"
  ClearErrors

cleanup_legacy_install:
  Delete "$INSTDIR\codex-plus-plus.exe"
  Delete "$INSTDIR\codex-plus-plus-manager.exe"
  Delete "$DESKTOP\mirror+.lnk"
  Delete "$DESKTOP\mirror+ 管理器.lnk"
  Delete "$DESKTOP\mirror+ 管理工具.lnk"
  Delete "$DESKTOP\mirror+ 绠＄悊宸ュ叿.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+ 管理器.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+ 管理工具.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+ 绠＄悊宸ュ叿.lnk"
  Delete "$SMPROGRAMS\mirror+\卸载 mirror+.lnk"
  RMDir "$SMPROGRAMS\mirror+"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorPlus"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Codex++"
  DeleteRegKey HKCU "Software\MirrorPlus"
  ClearErrors
  Goto install_done

staging_failed:
  Call CleanupStage
  SetErrorLevel 2
  IfSilent staging_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "安装文件无法完整解包，可能是磁盘空间不足或目录不可写。旧版本未被修改，请释放空间或更换目录后重试。"
staging_abort_silent:
  Abort

backup_cleanup_failed:
  Call CleanupStage
  SetErrorLevel 2
  IfSilent backup_cleanup_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "无法清理上次升级遗留的备份文件。旧版本未被修改；请完全退出相关程序、确认安装目录可写后重试。"
backup_cleanup_abort_silent:
  Abort

backup_failed:
  Call CleanupBackup
  Call CleanupStage
  SetErrorLevel 2
  IfSilent backup_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "无法完整建立升级备份，旧版本未被修改。请确认安装目录可写、磁盘空间充足并完全退出程序后重试。"
backup_abort_silent:
  Abort

install_rollback:
  Call RollbackUpgrade
  IfErrors rollback_failed_message
  SetErrorLevel 2
  IfSilent rollback_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "升级未完成，已自动恢复原程序文件。请检查磁盘空间和目录权限后重试。"
rollback_abort_silent:
  Abort

recovery_failed:
rollback_failed_message:
  SetErrorLevel 2
  IfSilent recovery_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "自动恢复未完成。为避免进一步覆盖，安装已中止；恢复文件保留在 $BackupDir，请勿删除并联系支持。"
recovery_abort_silent:
  Abort

install_done:
SectionEnd

Section "Uninstall"
  IfFileExists "$INSTDIR\mirror-x-codex.exe" 0 un_stop_current_manager
  Push "mirror-x-codex.exe"
  Call un.StopProductProcess
un_stop_current_manager:
  IfFileExists "$INSTDIR\mirror-x-codex-manager.exe" 0 un_stop_current_imagegen
  Push "mirror-x-codex-manager.exe"
  Call un.StopProductProcess
un_stop_current_imagegen:
  IfFileExists "$INSTDIR\mirror-x-imagegen.exe" 0 un_stop_legacy_launcher
  Push "mirror-x-imagegen.exe"
  Call un.StopProductProcess
un_stop_legacy_launcher:
  IfFileExists "$INSTDIR\codex-plus-plus.exe" 0 un_stop_legacy_manager
  Push "codex-plus-plus.exe"
  Call un.StopProductProcess
un_stop_legacy_manager:
  IfFileExists "$INSTDIR\codex-plus-plus-manager.exe" 0 un_product_processes_stopped
  Push "codex-plus-plus-manager.exe"
  Call un.StopProductProcess
un_product_processes_stopped:

  nsExec::ExecToLog '"$INSTDIR\mirror-x-codex-manager.exe" --restore-before-uninstall'
  Pop $0
  StrCmp $0 "0" restore_ok
  SetErrorLevel 2
  IfSilent restore_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "无法安全恢复 Codex 接入前状态，卸载已中止。请完全退出 Codex、确认系统盘至少有 128 MB 可用且配置目录可写后重试；程序和恢复数据均未删除。详情见 $PROFILE\.mirrorplus\codex-plus.log。"
restore_abort_silent:
  Abort
restore_ok:

  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "MirrorPlusWatcher"
  Delete "$SMSTARTUP\MirrorPlusWatcher.lnk"
  DeleteRegKey HKCU "Software\Classes\mirrorplus"

  ClearErrors
  Delete "$INSTDIR\mirror-x-codex.exe"
  IfErrors uninstall_files_failed
  Delete "$INSTDIR\mirror-x-imagegen.exe"
  IfErrors uninstall_files_failed
  Delete "$INSTDIR\codex-plus-plus.exe"
  IfErrors uninstall_files_failed
  Delete "$INSTDIR\codex-plus-plus-manager.exe"
  IfErrors uninstall_files_failed

  ClearErrors
  Delete "$DESKTOP\mirror+.lnk"
  Delete "$DESKTOP\mirror+ 管理器.lnk"
  Delete "$DESKTOP\mirror+ 管理工具.lnk"
  Delete "$DESKTOP\mirror+ 绠＄悊宸ュ叿.lnk"
  Delete "$DESKTOP\mirror x codex.lnk"
  Delete "$DESKTOP\mirror x codex 管理器.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+ 管理器.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+ 管理工具.lnk"
  Delete "$SMPROGRAMS\mirror+\mirror+ 绠＄悊宸ュ叿.lnk"
  Delete "$SMPROGRAMS\mirror+\卸载 mirror+.lnk"
  RMDir "$SMPROGRAMS\mirror+"
  Delete "$SMPROGRAMS\mirror x codex\mirror x codex.lnk"
  Delete "$SMPROGRAMS\mirror x codex\mirror x codex 管理器.lnk"
  Delete "$SMPROGRAMS\mirror x codex\卸载 mirror x codex.lnk"
  RMDir "$SMPROGRAMS\mirror x codex"
  IfErrors uninstall_files_failed

  StrCpy $StageDir "$INSTDIR\.mirror-x-update-stage"
  StrCpy $BackupDir "$INSTDIR\.mirror-x-update-backup"
  ClearErrors
  Delete "$StageDir\mirror-x-codex.exe"
  Delete "$StageDir\mirror-x-codex-manager.exe"
  Delete "$StageDir\mirror-x-imagegen.exe"
  Delete "$StageDir\uninstall.exe"
  RMDir "$StageDir"
  IfFileExists "$StageDir\*.*" uninstall_files_failed 0
  Delete "$BackupDir\transaction.pending"
  Delete "$BackupDir\transaction.backing-up"
  Delete "$BackupDir\mirror-x-codex.exe"
  Delete "$BackupDir\mirror-x-codex-manager.exe"
  Delete "$BackupDir\mirror-x-imagegen.exe"
  Delete "$BackupDir\uninstall.exe"
  RMDir "$BackupDir"
  IfFileExists "$BackupDir\*.*" uninstall_files_failed 0
  IfErrors uninstall_files_failed

  Delete "$INSTDIR\mirror-x-codex-manager.exe"
  IfErrors uninstall_files_failed

  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorXCodex"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\MirrorPlus"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Codex++"
  DeleteRegKey HKCU "Software\MirrorXCodex"
  DeleteRegKey HKCU "Software\MirrorPlus"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  ClearErrors
  Goto uninstall_done

uninstall_files_failed:
  SetErrorLevel 2
  IfSilent uninstall_abort_silent
  MessageBox MB_OK|MB_ICONSTOP "部分程序文件仍被占用或无法删除，卸载已中止。卸载入口和恢复数据均已保留，请退出相关程序后重试。"
uninstall_abort_silent:
  Abort

uninstall_done:
SectionEnd
