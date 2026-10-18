Unicode true
!define MULTIUSER_EXECUTIONLEVEL Highest
!define MULTIUSER_MUI
!define MULTIUSER_USE_PROGRAMFILES64
!define MULTIUSER_INSTALLMODE_DEFAULT_CURRENTUSER
!define MULTIUSER_INSTALLMODE_COMMANDLINE
!define MULTIUSER_INSTALLMODE_INSTDIR "LAN Remote Desktop"
!define MULTIUSER_INSTALLMODE_INSTDIR_REGISTRY_KEY "Software\LAN Remote Desktop"
!define MULTIUSER_INSTALLMODE_INSTDIR_REGISTRY_VALUENAME "InstallDir"
!include "MultiUser.nsh"
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!ifndef PAYLOAD
!error "Pass /DPAYLOAD=absolute/path/to/windows-host"
!endif
Name "LAN Remote Desktop"
OutFile "LAN-Remote-Desktop-Windows-Setup.exe"
InstallDir "$LOCALAPPDATA\Programs\LAN Remote Desktop"
ShowInstDetails show
ShowUninstDetails show
VIProductVersion "0.9.0.1"
VIAddVersionKey /LANG=1033 "ProductName" "LAN Remote Desktop"
VIAddVersionKey /LANG=1033 "FileVersion" "0.9.0-alpha.1"
VIAddVersionKey /LANG=1033 "FileDescription" "LAN Remote Desktop Host Installer"
VIAddVersionKey /LANG=1033 "LegalCopyright" "MIT License"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MULTIUSER_PAGE_INSTALLMODE
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Japanese"
!insertmacro MUI_LANGUAGE "English"
Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "Windows 11 x64 is required."
    Abort
  ${EndIf}
  ${IfNot} ${AtLeastWin11}
    MessageBox MB_ICONSTOP "Windows 11 is required."
    Abort
  ${EndIf}
  SetRegView 64
  !insertmacro MULTIUSER_INIT
FunctionEnd
Function un.onInit
  SetRegView 64
  !insertmacro MULTIUSER_UNINIT
FunctionEnd
Section "Host (required)" Main
  SectionIn RO
  ${If} $MultiUser.InstallMode == "AllUsers"
    ReadRegStr $0 HKCU "Software\LAN Remote Desktop" "InstallDir"
  ${Else}
    ReadRegStr $0 HKLM "Software\LAN Remote Desktop" "InstallDir"
  ${EndIf}
  ${If} $0 != ""
    MessageBox MB_ICONSTOP "Another installation mode exists. Uninstall it before changing mode. Pairing and user settings are retained."
    Abort
  ${EndIf}
  FindWindow $0 "LRD.Host.Tray.v1"
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Exit Host from its tray menu before installing. Active sessions must be disconnected first."
    Abort
  ${EndIf}
  SetOutPath "$INSTDIR"
  File "${PAYLOAD}\lrd-windows-host.exe"
  File "${PAYLOAD}\Start-Host.ps1"
  File "${PAYLOAD}\Test-Host.ps1"
  File "${PAYLOAD}\Configure-Firewall.ps1"
  File "${PAYLOAD}\manual-testing.md"
  File "${PAYLOAD}\README.txt"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr SHCTX "Software\LAN Remote Desktop" "InstallDir" "$INSTDIR"
  WriteRegStr SHCTX "Software\LAN Remote Desktop" "InstallMode" "$MultiUser.InstallMode"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\LANRemoteDesktop" "DisplayName" "LAN Remote Desktop"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\LANRemoteDesktop" "DisplayVersion" "0.9.0-alpha.1"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\LANRemoteDesktop" "InstallLocation" "$INSTDIR"
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\LANRemoteDesktop" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\" /$MultiUser.InstallMode'
  WriteRegDWORD SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\LANRemoteDesktop" "NoModify" 1
  WriteRegDWORD SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\LANRemoteDesktop" "NoRepair" 1
  CreateDirectory "$SMPROGRAMS\LAN Remote Desktop"
  CreateShortcut "$SMPROGRAMS\LAN Remote Desktop\Host.lnk" "$INSTDIR\lrd-windows-host.exe" "--serve --auto"
  CreateShortcut "$SMPROGRAMS\LAN Remote Desktop\Ethernet setup.lnk" "$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" '-NoProfile -File "$INSTDIR\Start-Host.ps1"'
  CreateShortcut "$SMPROGRAMS\LAN Remote Desktop\Uninstall.lnk" "$INSTDIR\Uninstall.exe" "/$MultiUser.InstallMode"
SectionEnd
Section /o "Start Host at user logon" Startup
  ; HKLM Run launches within each logged-in user, never a Session 0 service.
  WriteRegStr SHCTX "Software\Microsoft\Windows\CurrentVersion\Run" "LANRemoteDesktop" '$\"$INSTDIR\lrd-windows-host.exe$\" --serve --auto'
SectionEnd
Section "Uninstall"
  FindWindow $0 "LRD.Host.Tray.v1"
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "Exit Host from its tray menu before uninstalling."
    Abort
  ${EndIf}
  DeleteRegValue SHCTX "Software\Microsoft\Windows\CurrentVersion\Run" "LANRemoteDesktop"
  DeleteRegKey SHCTX "Software\Microsoft\Windows\CurrentVersion\Uninstall\LANRemoteDesktop"
  DeleteRegKey SHCTX "Software\LAN Remote Desktop"
  Delete "$SMPROGRAMS\LAN Remote Desktop\Host.lnk"
  Delete "$SMPROGRAMS\LAN Remote Desktop\Ethernet setup.lnk"
  Delete "$SMPROGRAMS\LAN Remote Desktop\Uninstall.lnk"
  RMDir "$SMPROGRAMS\LAN Remote Desktop"
  Delete "$INSTDIR\lrd-windows-host.exe"
  Delete "$INSTDIR\Start-Host.ps1"
  Delete "$INSTDIR\Test-Host.ps1"
  Delete "$INSTDIR\Configure-Firewall.ps1"
  Delete "$INSTDIR\manual-testing.md"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
SectionEnd
