; Managed-runtime adaptation of the reviewed per-user NSIS 3.12 / MUI2 template.
; Product identity and payload names remain validated build inputs. The native
; helper deliberately owns immutable-runtime lifecycle, PATH and Installed
; Apps state, quiet uninstall, and final root cleanup. NSIS owns wizard flow,
; exact option parsing, embedded inputs, cancellation, progress, and terminal UI.
!ifndef ARG_STAGE_DIR
  !error "ARG_STAGE_DIR is required"
!endif
!ifndef ARG_LAUNCHER_EXE
  !error "ARG_LAUNCHER_EXE is required"
!endif
!ifndef ARG_HELPER_EXE
  !error "ARG_HELPER_EXE is required"
!endif
!ifndef ARG_SKILL_MD
  !error "ARG_SKILL_MD is required"
!endif
!ifndef ARG_SKILL_HASH_MANIFEST
  !error "ARG_SKILL_HASH_MANIFEST is required"
!endif
!ifndef ARG_ARTWORK_DIR
  !error "ARG_ARTWORK_DIR is required"
!endif
!ifndef APP_BUILD_ID
  !error "APP_BUILD_ID is required"
!endif
!ifndef INFO_PRODUCTVERSION_DISPLAY
  !error "INFO_PRODUCTVERSION_DISPLAY is required"
!endif
!ifndef INFO_PRODUCTVERSION_FIXED
  !error "INFO_PRODUCTVERSION_FIXED is required"
!endif
!ifndef INFO_PRODUCTVERSION_UI
  !error "INFO_PRODUCTVERSION_UI is required"
!endif
!ifndef INFO_UPSTREAMVERSION
  !error "INFO_UPSTREAMVERSION is required"
!endif
!ifndef APP_OUTPUT_PATH
  !error "APP_OUTPUT_PATH is required"
!endif
!ifndef INFO_PRODUCTNAME
  !error "INFO_PRODUCTNAME is required"
!endif
!ifndef INFO_DISTRIBUTIONNAME
  !error "INFO_DISTRIBUTIONNAME is required"
!endif
!ifndef INFO_COMPANYNAME
  !error "INFO_COMPANYNAME is required"
!endif
!ifndef INFO_COPYRIGHT
  !error "INFO_COPYRIGHT is required"
!endif
!ifndef INFO_PRODUCTURL
  !error "INFO_PRODUCTURL is required"
!endif
!ifndef INFO_UPSTREAMURL
  !error "INFO_UPSTREAMURL is required"
!endif
!ifndef INFO_COMMANDNAME
  !error "INFO_COMMANDNAME is required"
!endif
!ifndef INFO_ORIGINALFILENAME
  !error "INFO_ORIGINALFILENAME is required"
!endif
!ifndef APP_START_GATE_ENV
  !error "APP_START_GATE_ENV is required"
!endif
!ifndef APP_TEST_MARKER_PREFIX
  !error "APP_TEST_MARKER_PREFIX is required"
!endif

Unicode true
!define APP_LANG_ENGLISH 1033
!define APP_ENVIRONMENT_BROADCAST_TIMEOUT_MS 250
!define APP_EXIT_INVALID_ARGUMENTS 30
!define APP_EXIT_UNSUPPORTED_PLATFORM 50
!define APP_EXIT_INSTALL_FAILED 70
!define APP_EXIT_UNINSTALL_FAILED 80

Name "${INFO_DISTRIBUTIONNAME}"
Caption "${INFO_DISTRIBUTIONNAME} Setup"
OutFile "${APP_OUTPUT_PATH}"
InstallDir "$LOCALAPPDATA\Programs\${INFO_PRODUCTNAME}"
RequestExecutionLevel user
CRCCheck force
SetCompressor lzma
SetDatablockOptimize on
SetCompressorDictSize 8
SetCompressor /SOLID /FINAL lzma
AllowSkipFiles off
ShowInstDetails show
ShowUninstDetails show
AutoCloseWindow true
ManifestDPIAware true
ManifestSupportedOS all
VIProductVersion "${INFO_PRODUCTVERSION_FIXED}"
VIFileVersion "${INFO_PRODUCTVERSION_FIXED}"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "ProductName" "${INFO_DISTRIBUTIONNAME}"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "CompanyName" "${INFO_COMPANYNAME}"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "LegalCopyright" "${INFO_COPYRIGHT}"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "FileDescription" "${INFO_DISTRIBUTIONNAME} per-user installer"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "FileVersion" "${INFO_PRODUCTVERSION_DISPLAY}"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "ProductVersion" "${INFO_PRODUCTVERSION_DISPLAY}"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "UpstreamVersion" "${INFO_UPSTREAMVERSION}"
VIAddVersionKey /LANG=${APP_LANG_ENGLISH} "OriginalFilename" "${INFO_ORIGINALFILENAME}"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "nsDialogs.nsh"
!include "WinMessages.nsh"
!include "x64.nsh"

Var HelperExitCode
Var HelperOutput
Var FailureExitCode
Var StartGate
Var InstallManager
Var SettingsDisposition
Var SettingsCheckbox
Var SkillDisposition
Var SkillCheckbox
Var UpstreamLink
Var QuietRunnerPid
Var QuietToken
Var QuietHelperArgs
Var InstallMutationActive

!define INSTALLER_WELCOME_BITMAP_100 "${ARG_ARTWORK_DIR}\installer-welcome-finish-164x314.bmp"
!define INSTALLER_WELCOME_BITMAP_125 "${ARG_ARTWORK_DIR}\installer-welcome-finish-205x393.bmp"
!define INSTALLER_WELCOME_BITMAP_150 "${ARG_ARTWORK_DIR}\installer-welcome-finish-246x471.bmp"
!define INSTALLER_WELCOME_BITMAP_175 "${ARG_ARTWORK_DIR}\installer-welcome-finish-287x550.bmp"
!define INSTALLER_WELCOME_BITMAP_200 "${ARG_ARTWORK_DIR}\installer-welcome-finish-328x628.bmp"

!define MUI_ABORTWARNING
!define MUI_CUSTOMFUNCTION_ABORT PreventInstallMutationAbort
!define MUI_CUSTOMFUNCTION_UNABORT un.PreventUninstallMutationAbort
!define MUI_WELCOMEFINISHPAGE_BITMAP "${INSTALLER_WELCOME_BITMAP_100}"
!define MUI_WELCOMEFINISHPAGE_BITMAP_STRETCH NoStretchNoCropNoAlign
!define MUI_CUSTOMFUNCTION_GUIINIT SelectInstallerWelcomeBitmap
!pragma verifyloadimage "${INSTALLER_WELCOME_BITMAP_125}"
!pragma verifyloadimage "${INSTALLER_WELCOME_BITMAP_150}"
!pragma verifyloadimage "${INSTALLER_WELCOME_BITMAP_175}"
!pragma verifyloadimage "${INSTALLER_WELCOME_BITMAP_200}"
!define MUI_WELCOMEPAGE_TITLE "Install ${INFO_DISTRIBUTIONNAME} ${INFO_PRODUCTVERSION_UI}"
!define MUI_WELCOMEPAGE_TEXT "This setup installs ${INFO_DISTRIBUTIONNAME}, an unofficial Windows distribution of ${INFO_PRODUCTNAME}. It advances Windows support by applying this fork's patches to the latest reviewed stable ${INFO_PRODUCTNAME} ${INFO_UPSTREAMVERSION} release.$\r$\n$\r$\nNo administrator access is required. Open a new terminal after setup so it can find ${INFO_COMMANDNAME} on PATH."
!define MUI_FINISHPAGE_NOREBOOTSUPPORT
!define MUI_FINISHPAGE_TEXT_LARGE
!define MUI_FINISHPAGE_TITLE "${INFO_DISTRIBUTIONNAME} ${INFO_PRODUCTVERSION_UI} is installed"
!define MUI_FINISHPAGE_TEXT "Setup completed successfully.$\r$\n$\r$\n${INFO_DISTRIBUTIONNAME} is an unofficial distribution; the command remains ${INFO_COMMANDNAME} and no application window opens.$\r$\n$\r$\nOpen a new terminal, then run:$\r$\n${INFO_COMMANDNAME}"
!define MUI_FINISHPAGE_LINK "Open ${INFO_DISTRIBUTIONNAME} setup and usage guide"
!define MUI_FINISHPAGE_LINK_LOCATION "${INFO_PRODUCTURL}"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${ARG_STAGE_DIR}\LICENSE.txt"
!insertmacro MUI_PAGE_INSTFILES
!define MUI_PAGE_CUSTOMFUNCTION_SHOW PositionInstallerFinishLink
!insertmacro MUI_PAGE_FINISH

UninstPage custom un.SettingsPage un.SettingsPageLeave
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

LangString AppSettingsPageTitle ${LANG_ENGLISH} "Remove local ${INFO_DISTRIBUTIONNAME} data"
LangString AppSettingsPageSubtitle ${LANG_ENGLISH} "Choose what remains after uninstall."
LangString AppSettingsPageText ${LANG_ENGLISH} "Uninstall stops running managed sessions, then removes the managed program, user PATH entry, and Windows Installed Apps registration. Unmodified skill copies are selected for removal; customized copies are kept unless you select skill removal. Other files in those skill folders are never removed. Settings and session data are also kept unless selected."
LangString AppSkillCheckbox ${LANG_ENGLISH} "Remove installed ${INFO_PRODUCTNAME} skill copies, including customized SKILL.md files"
LangString AppSettingsCheckbox ${LANG_ENGLISH} "Also delete ${INFO_PRODUCTNAME} settings and session data"
LangString AppDetailRemoveSettings ${LANG_ENGLISH} "Removing ${INFO_PRODUCTNAME} settings and session data..."

!ifdef TEST_UNINSTALL_FAULT
  !define APP_UNINSTALL_FAULT_ARGS '--uninstall-fault "${TEST_UNINSTALL_FAULT}" --fault-marker-prefix "${APP_TEST_MARKER_PREFIX}"'
!else
  !define APP_UNINSTALL_FAULT_ARGS ""
!endif
!ifdef TEST_INSTALL_FAULT
  !define APP_INSTALL_FAULT_ARGS '--install-fault "install-${TEST_INSTALL_FAULT}" --fault-marker-prefix "${APP_TEST_MARKER_PREFIX}"'
!else
  !define APP_INSTALL_FAULT_ARGS ""
!endif
!ifdef TEST_USER_PROFILE_ROOT
  !define APP_USER_PROFILE_ROOT "${TEST_USER_PROFILE_ROOT}"
!else
  !define APP_USER_PROFILE_ROOT "$PROFILE"
!endif

Function NotifyEnvironmentChange
  ; WM_SETTINGCHANGE officially uses SendMessageTimeout with HWND_BROADCAST.
  ; Its timeout applies to every top-level window, so keep each unrelated hung
  ; window tightly bounded instead of delaying setup before Finish.
  System::Call 'USER32::SendMessageTimeoutW(p 0xffff, i ${WM_SETTINGCHANGE}, p 0, w "Environment", i 0x2, i ${APP_ENVIRONMENT_BROADCAST_TIMEOUT_MS}, *p .r0)'
FunctionEnd

Function un.NotifyEnvironmentChange
  System::Call 'USER32::SendMessageTimeoutW(p 0xffff, i ${WM_SETTINGCHANGE}, p 0, w "Environment", i 0x2, i ${APP_ENVIRONMENT_BROADCAST_TIMEOUT_MS}, *p .r0)'
FunctionEnd

Function FailInstall
  Exch $0
  DetailPrint "$0"
  !ifdef TEST_UNINSTALL_FAULT
    FileOpen $1 "$TEMP\${APP_TEST_MARKER_PREFIX}-install-failure-${TEST_UNINSTALL_FAULT}.txt" w
    FileWrite $1 "$0"
    FileClose $1
  !endif
  IfSilent install_failure_silent
  MessageBox MB_OK|MB_ICONSTOP "$0"
install_failure_silent:
  SetErrorLevel $FailureExitCode
  Quit
FunctionEnd

Function PreventInstallMutationAbort
  ${If} $InstallMutationActive == "1"
    MessageBox MB_ICONEXCLAMATION|MB_OK "${INFO_DISTRIBUTIONNAME} is completing or rolling back installation changes. Wait for setup to finish." /SD IDOK
    Abort
  ${EndIf}
FunctionEnd

Function un.PreventUninstallMutationAbort
  ${If} $InstallMutationActive == "1"
    MessageBox MB_ICONEXCLAMATION|MB_OK "${INFO_DISTRIBUTIONNAME} is cleaning or restoring installed state. Wait for uninstall to finish." /SD IDOK
    Abort
  ${EndIf}
FunctionEnd

Function DisableInstallCancellation
  IfSilent install_cancel_disabled
  GetDlgItem $0 $HWNDPARENT 2
  EnableWindow $0 0
install_cancel_disabled:
FunctionEnd

Function EnableInstallCancellation
  IfSilent install_cancel_enabled
  GetDlgItem $0 $HWNDPARENT 2
  EnableWindow $0 1
install_cancel_enabled:
FunctionEnd

Function un.DisableUninstallCancellation
  IfSilent uninstall_cancel_disabled
  GetDlgItem $0 $HWNDPARENT 2
  EnableWindow $0 0
uninstall_cancel_disabled:
FunctionEnd

Function un.FailUninstall
  Exch $0
  DetailPrint "$0"
  IfSilent uninstall_failure_silent
  MessageBox MB_OK|MB_ICONSTOP "$0"
uninstall_failure_silent:
  SetErrorLevel $FailureExitCode
  Quit
FunctionEnd

Function WaitForUpdaterStartGate
  ReadEnvStr $StartGate "${APP_START_GATE_ENV}"
  StrCmp $StartGate "" updater_start_gate_done
  StrCpy $0 "0"
updater_start_gate_loop:
  IfFileExists "$StartGate" updater_start_gate_ready
  IntCmp $0 600 updater_start_gate_timeout updater_start_gate_sleep updater_start_gate_timeout
updater_start_gate_sleep:
  Sleep 50
  IntOp $0 $0 + 1
  Goto updater_start_gate_loop
updater_start_gate_ready:
  Delete "$StartGate"
  Goto updater_start_gate_done
updater_start_gate_timeout:
  Push "Timed out waiting for the verified updater process boundary."
  Call FailInstall
updater_start_gate_done:
FunctionEnd

Function SelectInstallerWelcomeBitmap
  InitPluginsDir
  System::Call 'KERNEL32::GetModuleHandleW(w "USER32.DLL") p.r0'
  System::Call 'KERNEL32::GetProcAddress(p r0, m "GetDpiForWindow") p.r1'
  ${If} $1 == 0
    StrCpy $0 96
  ${Else}
    System::Call '::$1(p $HWNDPARENT)i.r0'
    ${If} $0 == 0
      StrCpy $0 96
    ${EndIf}
  ${EndIf}
  ${If} $0 >= 180
    File "/oname=$PLUGINSDIR\modern-wizard.bmp" "${INSTALLER_WELCOME_BITMAP_200}"
  ${ElseIf} $0 >= 156
    File "/oname=$PLUGINSDIR\modern-wizard.bmp" "${INSTALLER_WELCOME_BITMAP_175}"
  ${ElseIf} $0 >= 132
    File "/oname=$PLUGINSDIR\modern-wizard.bmp" "${INSTALLER_WELCOME_BITMAP_150}"
  ${ElseIf} $0 >= 108
    File "/oname=$PLUGINSDIR\modern-wizard.bmp" "${INSTALLER_WELCOME_BITMAP_125}"
  ${EndIf}
FunctionEnd

Function PositionInstallerFinishLink
  System::Store "S"
  ${NSD_GetText} $mui.FinishPage.Text $0
  System::Call 'USER32::GetWindowRect(p $mui.FinishPage.Text, @r1)'
  System::Call '*$1(i.r2, i.r3, i.r4, i.r5)'
  IntOp $4 $4 - $2
  System::Call '*$1(i 0, i 0, i r4, i 0)'
  System::Call 'USER32::GetDC(p $mui.FinishPage.Text) p.r6'
  SendMessage $mui.FinishPage.Text ${WM_GETFONT} 0 0 $7
  System::Call 'GDI32::SelectObject(p r6, p r7) p.s'
  System::Call 'USER32::DrawTextW(p r6, w r0, i -1, p r1, i 0x00000C10)'
  System::Call '*$1(i, i, i, i.r8)'
  System::Call 'GDI32::SelectObject(p r6, p s)'
  System::Call 'USER32::ReleaseDC(p $mui.FinishPage.Text, p r6)'

  System::Call 'USER32::GetWindowRect(p $mui.FinishPage.Text, @r1)'
  System::Call 'USER32::MapWindowPoints(p 0, p $mui.FinishPage, p r1, i 2)'
  System::Call '*$1(i.r2, i.r3, i.r4, i.r5)'
  IntOp $7 $4 - $2
  System::Call 'USER32::SetWindowPos(p $mui.FinishPage.Text, p 0, i r2, i r3, i r7, i r8, i 0x14)'

  System::Call 'USER32::GetWindowRect(p $mui.FinishPage.Link, @r1)'
  System::Call 'USER32::MapWindowPoints(p 0, p $mui.FinishPage, p r1, i 2)'
  System::Call '*$1(i.r2, i.r4, i.r5, i.r6)'
  IntOp $7 $5 - $2
  IntOp $9 $6 - $4
  IntOp $8 $8 + $3
  IntOp $8 $8 + $9
  System::Call 'USER32::SetWindowPos(p $mui.FinishPage.Link, p 0, i r2, i r8, i r7, i r9, i 0x14)'

  ${NSD_CreateLink} 120u 185u 195u 10u "Open official ${INFO_PRODUCTNAME} project"
  Pop $UpstreamLink
  SetCtlColors $UpstreamLink "000080" "FFFFFF"
  ${NSD_OnClick} $UpstreamLink OpenInstallerUpstream
  IntOp $8 $8 + $9
  IntOp $8 $8 + 2
  System::Call 'USER32::SetWindowPos(p $UpstreamLink, p 0, i r2, i r8, i r7, i r9, i 0x14)'
  System::Store "L"
FunctionEnd

Function OpenInstallerUpstream
  ExecShell open "${INFO_UPSTREAMURL}"
FunctionEnd

Function .onInit
  SetShellVarContext current
  StrCpy $FailureExitCode ${APP_EXIT_INSTALL_FAILED}
  StrCpy $InstallManager "Direct"
  StrCpy $InstallMutationActive "0"
  !ifdef TEST_USER_PROFILE_ROOT
    StrCpy $INSTDIR "${TEST_USER_PROFILE_ROOT}\AppData\Local\Programs\${INFO_PRODUCTNAME}"
  !else
  StrCmp $PROFILE "" 0 installer_profile_ready
  Push "The current Windows user profile is unavailable; setup did not change this computer."
  Call FailInstall
installer_profile_ready:
  StrCmp $LOCALAPPDATA "" installer_local_appdata_fallback installer_local_appdata_set
installer_local_appdata_fallback:
  StrCpy $INSTDIR "$PROFILE\AppData\Local\Programs\${INFO_PRODUCTNAME}"
  Goto installer_local_appdata_ready
installer_local_appdata_set:
  StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${INFO_PRODUCTNAME}"
installer_local_appdata_ready:
  !endif
  ${GetParameters} $0
  StrCmp $0 "" installer_arguments_ready
  StrCmp $0 "/S" installer_arguments_ready
  StrCmp $0 "/WINGET" installer_winget_arguments
  StrCmp $0 "/S /WINGET" installer_winget_arguments
  StrCmp $0 "/WINGET /S" installer_winget_arguments
  StrCpy $FailureExitCode ${APP_EXIT_INVALID_ARGUMENTS}
  Push "Unsupported setup arguments. Use only /S and the exact /WINGET option."
  Call FailInstall
installer_winget_arguments:
  StrCpy $InstallManager "WinGet"
installer_arguments_ready:
  ${IfNot} ${RunningX64}
    StrCpy $FailureExitCode ${APP_EXIT_UNSUPPORTED_PLATFORM}
    Push "${INFO_DISTRIBUTIONNAME} requires 64-bit Windows."
    Call FailInstall
  ${EndIf}
  Call WaitForUpdaterStartGate
FunctionEnd

Function un.onInit
  SetShellVarContext current
  StrCpy $FailureExitCode ${APP_EXIT_UNINSTALL_FAILED}
  StrCmp $PROFILE "" 0 uninstaller_profile_ready
  Push "The current Windows user profile is unavailable; uninstall preserved the existing installation."
  Call un.FailUninstall
uninstaller_profile_ready:
  StrCpy $SettingsDisposition "Keep"
  StrCpy $SkillDisposition "Auto"
  StrCpy $QuietRunnerPid ""
  StrCpy $QuietToken ""
  StrCpy $QuietHelperArgs ""
  StrCpy $InstallMutationActive "0"
  ${GetParameters} $0
  StrCmp $0 "" un_arguments_ready
  StrCmp $0 "/S" un_arguments_ready
  StrCmp $0 "/REMOVE_SETTINGS" un_arguments_settings
  StrCmp $0 "/S /REMOVE_SETTINGS" un_arguments_settings
  StrCmp $0 "/REMOVE_SETTINGS /S" un_arguments_settings
  StrCmp $0 "/REMOVE_SKILL" un_arguments_skill
  StrCmp $0 "/S /REMOVE_SKILL" un_arguments_skill
  StrCmp $0 "/REMOVE_SKILL /S" un_arguments_skill
  StrCmp $0 "/REMOVE_SETTINGS /REMOVE_SKILL" un_arguments_both
  StrCmp $0 "/REMOVE_SKILL /REMOVE_SETTINGS" un_arguments_both
  StrCmp $0 "/S /REMOVE_SETTINGS /REMOVE_SKILL" un_arguments_both
  StrCmp $0 "/S /REMOVE_SKILL /REMOVE_SETTINGS" un_arguments_both
  StrCmp $0 "/REMOVE_SETTINGS /S /REMOVE_SKILL" un_arguments_both
  StrCmp $0 "/REMOVE_SETTINGS /REMOVE_SKILL /S" un_arguments_both
  StrCmp $0 "/REMOVE_SKILL /S /REMOVE_SETTINGS" un_arguments_both
  StrCmp $0 "/REMOVE_SKILL /REMOVE_SETTINGS /S" un_arguments_both
  Goto un_quiet_probe
un_arguments_settings:
  StrCpy $SettingsDisposition "Remove"
  Goto un_arguments_ready
un_arguments_skill:
  StrCpy $SkillDisposition "Remove"
  Goto un_arguments_ready
un_arguments_both:
  StrCpy $SettingsDisposition "Remove"
  StrCpy $SkillDisposition "Remove"
  Goto un_arguments_ready
un_quiet_probe:
  ClearErrors
  ${GetOptionsS} "$0" "/NATIVE_QUIET_RUNNER_PID=" $QuietRunnerPid
  ${If} ${Errors}
    Goto un_arguments_invalid
  ${EndIf}
  ClearErrors
  ${GetOptionsS} "$0" "/NATIVE_QUIET_TOKEN=" $QuietToken
  ${If} ${Errors}
    Goto un_arguments_invalid
  ${EndIf}
  StrCpy $1 "/S /NATIVE_QUIET_RUNNER_PID=$QuietRunnerPid /NATIVE_QUIET_TOKEN=$QuietToken"
  StrCmp $0 $1 un_quiet_ready un_arguments_invalid
un_arguments_invalid:
  StrCpy $FailureExitCode ${APP_EXIT_INVALID_ARGUMENTS}
  Push "Unsupported uninstall arguments. Use only /S, /REMOVE_SETTINGS, /REMOVE_SKILL, or the native quiet-uninstall rendezvous."
  Call un.FailUninstall
un_quiet_ready:
  StrCpy $QuietHelperArgs '--quiet-runner-process-id "$QuietRunnerPid" --quiet-token "$QuietToken"'
un_arguments_ready:
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  ClearErrors
  File /oname=installer-helper.exe "${ARG_HELPER_EXE}"
  File /oname=managed-skill-hashes.txt "${ARG_SKILL_HASH_MANIFEST}"
  IfErrors 0 un_skill_manifest_ready
  Push "The managed uninstall helper or skill ownership manifest could not be unpacked; uninstall was preserved."
  Call un.FailUninstall
un_skill_manifest_ready:
FunctionEnd

Function un.SettingsPage
  IfSilent settings_page_done 0
  ${If} $SkillDisposition == "Auto"
    StrCpy $SkillDisposition "Keep"
    IfFileExists "$INSTDIR\state\installer-helper.exe" 0 skill_default_done
    nsExec::ExecToStack /TIMEOUT=30000 '"$INSTDIR\state\installer-helper.exe" skill-removal-default --user-profile-root "${APP_USER_PROFILE_ROOT}" --skill-hash-manifest "$PLUGINSDIR\managed-skill-hashes.txt"'
    Pop $HelperExitCode
    Pop $HelperOutput
    StrCmp $HelperExitCode "0" 0 skill_default_done
    StrCmp $HelperOutput "Remove" 0 skill_default_done
    StrCpy $SkillDisposition "Remove"
skill_default_done:
  ${EndIf}
  !insertmacro MUI_HEADER_TEXT "$(AppSettingsPageTitle)" "$(AppSettingsPageSubtitle)"
  nsDialogs::Create 1018
  Pop $0
  ${If} $0 == error
    Abort
  ${EndIf}

  ${NSD_CreateLabel} 0 0 100% 66u "$(AppSettingsPageText)"
  Pop $0
  ${NSD_CreateCheckbox} 0 70u 100% 18u "$(AppSkillCheckbox)"
  Pop $SkillCheckbox
  ${If} $SkillDisposition == "Remove"
    ${NSD_Check} $SkillCheckbox
  ${EndIf}
  ${NSD_CreateCheckbox} 0 92u 100% 14u "$(AppSettingsCheckbox)"
  Pop $SettingsCheckbox
  ${If} $SettingsDisposition == "Remove"
    ${NSD_Check} $SettingsCheckbox
  ${EndIf}
  nsDialogs::Show
settings_page_done:
FunctionEnd

Function un.SettingsPageLeave
  IfSilent settings_page_leave_done 0
  ${If} $SettingsCheckbox == ""
    Goto settings_page_leave_done
  ${EndIf}

  ${NSD_GetState} $SkillCheckbox $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $SkillDisposition "Remove"
  ${Else}
    StrCpy $SkillDisposition "Keep"
  ${EndIf}
  ${NSD_GetState} $SettingsCheckbox $0
  ${If} $0 == ${BST_CHECKED}
    StrCpy $SettingsDisposition "Remove"
  ${Else}
    StrCpy $SettingsDisposition "Keep"
  ${EndIf}
settings_page_leave_done:
FunctionEnd

Section "${INFO_DISTRIBUTIONNAME}" SEC_APP
  SectionIn RO
  InitPluginsDir
  ClearErrors
  SetOutPath "$PLUGINSDIR\payload"
  File /r "${ARG_STAGE_DIR}\*"
  SetOutPath "$PLUGINSDIR"
  File /oname=app-launcher.exe "${ARG_LAUNCHER_EXE}"
  File /oname=installer-helper.exe "${ARG_HELPER_EXE}"
  SetOutPath "$PLUGINSDIR\skill"
  File /oname=SKILL.md "${ARG_SKILL_MD}"
  File /oname=managed-skill-hashes.txt "${ARG_SKILL_HASH_MANIFEST}"
  SetOutPath "$PLUGINSDIR"
  WriteUninstaller "$PLUGINSDIR\uninstall.exe"
  IfErrors 0 installer_inputs_ready
  Push "${INFO_DISTRIBUTIONNAME} setup could not unpack its embedded, pre-verified files."
  Call FailInstall

installer_inputs_ready:
  DetailPrint "Validating and activating ${INFO_DISTRIBUTIONNAME} ${INFO_PRODUCTVERSION_UI}..."
  ; Deliberate managed-runtime deviation: setup publishes a pending immutable
  ; runtime instead of stopping active sessions. Future launches activate it.
  StrCpy $InstallMutationActive "1"
  Call DisableInstallCancellation
  nsExec::ExecToStack /TIMEOUT=180000 '"$PLUGINSDIR\installer-helper.exe" install --install-root "$INSTDIR" --user-profile-root "${APP_USER_PROFILE_ROOT}" --package-root "$PLUGINSDIR" --build-id "${APP_BUILD_ID}" --display-version "${INFO_PRODUCTVERSION_DISPLAY}" --numeric-version "${INFO_PRODUCTVERSION_FIXED}" --install-manager "$InstallManager" ${APP_INSTALL_FAULT_ARGS}'
  Pop $HelperExitCode
  Pop $HelperOutput
  StrCmp $HelperExitCode "error" installer_helper_start_failed
  StrCmp $HelperExitCode "timeout" installer_helper_timed_out
  StrCmp $HelperExitCode "0" installer_complete
  StrCpy $0 "${INFO_DISTRIBUTIONNAME} setup failed ($HelperExitCode). $HelperOutput"
  Push $0
  Call FailInstall
installer_helper_start_failed:
  Push "${INFO_DISTRIBUTIONNAME} setup could not start its native installer helper."
  Call FailInstall
installer_helper_timed_out:
  Push "${INFO_DISTRIBUTIONNAME} setup exceeded its 180 second installer-helper deadline."
  Call FailInstall

installer_complete:
  StrCpy $InstallMutationActive "0"
  Call EnableInstallCancellation
  DetailPrint "$HelperOutput"
  Call NotifyEnvironmentChange
  SetErrorLevel 0
SectionEnd

Section "Uninstall"
  SetAutoClose true
  ; The uninstaller carries its own native helper so every retry uses one
  ; validation and lifecycle-lock owner. That helper stops managed sessions
  ; under the installer mutex before taking the launch gate or mutating state.
  DetailPrint "Stopping running ${INFO_DISTRIBUTIONNAME} sessions before uninstall..."
  StrCpy $InstallMutationActive "1"
  Call un.DisableUninstallCancellation
  nsExec::ExecToStack /TIMEOUT=180000 '"$PLUGINSDIR\installer-helper.exe" uninstall --install-root "$INSTDIR" --user-profile-root "${APP_USER_PROFILE_ROOT}" --settings-disposition "$SettingsDisposition" --skill-hash-manifest "$PLUGINSDIR\managed-skill-hashes.txt" --skill-disposition "$SkillDisposition" ${APP_UNINSTALL_FAULT_ARGS} $QuietHelperArgs'
  Pop $HelperExitCode
  Pop $HelperOutput
  StrCmp $HelperExitCode "error" un_helper_start_failed
  StrCmp $HelperExitCode "timeout" un_helper_timed_out
  StrCmp $HelperExitCode "0" un_helper_complete
  StrCpy $0 "${INFO_DISTRIBUTIONNAME} uninstall failed ($HelperExitCode). $HelperOutput"
  Push $0
  Call un.FailUninstall
un_helper_start_failed:
  Push "${INFO_DISTRIBUTIONNAME} uninstall could not start its native installer helper."
  Call un.FailUninstall
un_helper_timed_out:
  Push "${INFO_DISTRIBUTIONNAME} uninstall exceeded its 180 second installer-helper deadline."
  Call un.FailUninstall

un_helper_complete:
  StrCpy $InstallMutationActive "0"
  DetailPrint "$HelperOutput"
  Call un.NotifyEnvironmentChange
  ${If} $SettingsDisposition == "Remove"
    DetailPrint "$(AppDetailRemoveSettings)"
  ${EndIf}
  IfFileExists "$INSTDIR\." uninstall_cleanup_failed uninstall_complete
uninstall_cleanup_failed:
  Push "${INFO_DISTRIBUTIONNAME} uninstall helper returned before removing its validated install root. Retry uninstall."
  Call un.FailUninstall
uninstall_complete:
  SetErrorLevel 0
SectionEnd
