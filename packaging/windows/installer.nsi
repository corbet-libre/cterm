; cterm Windows Installer Script
; NSIS Modern User Interface

!include "MUI2.nsh"

; General
Name "cterm"
OutFile "cterm-${VERSION}-setup.exe"
InstallDir "$PROGRAMFILES64\cterm"
InstallDirRegKey HKLM "Software\cterm" "InstallDir"
RequestExecutionLevel admin

; Icon settings - use generated icon
!define MUI_ICON "cterm.ico"
!define MUI_UNICON "cterm.ico"

; Interface Settings
!define MUI_ABORTWARNING

; Pages
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

; Languages
!insertmacro MUI_LANGUAGE "English"

; Installer Section
Section "Install"
    SetOutPath "$INSTDIR"

    ; Copy all files from the build directory
    File /r "cterm-windows-x86_64\*.*"

    ; Copy icon
    File "cterm.ico"

    ; Create uninstaller
    WriteUninstaller "$INSTDIR\uninstall.exe"

    ; Create Start Menu shortcuts
    CreateDirectory "$SMPROGRAMS\cterm"
    CreateShortcut "$SMPROGRAMS\cterm\cterm.lnk" "$INSTDIR\cterm.exe" "" "$INSTDIR\cterm.ico"
    CreateShortcut "$SMPROGRAMS\cterm\Uninstall.lnk" "$INSTDIR\uninstall.exe"

    ; Create Desktop shortcut
    CreateShortcut "$DESKTOP\cterm.lnk" "$INSTDIR\cterm.exe" "" "$INSTDIR\cterm.ico"

    ; Write registry keys for uninstaller
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm" "DisplayName" "cterm"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm" "UninstallString" "$INSTDIR\uninstall.exe"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm" "DisplayIcon" "$INSTDIR\cterm.ico"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm" "Publisher" "cterm contributors"
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm" "DisplayVersion" "${VERSION}"
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm" "NoModify" 1
    WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm" "NoRepair" 1

    ; Store install directory
    WriteRegStr HKLM "Software\cterm" "InstallDir" "$INSTDIR"
SectionEnd

; Uninstaller Section
Section "Uninstall"
    ; Remove files
    RMDir /r "$INSTDIR"

    ; Remove Start Menu shortcuts
    RMDir /r "$SMPROGRAMS\cterm"

    ; Remove Desktop shortcut
    Delete "$DESKTOP\cterm.lnk"

    ; Remove registry keys
    DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\cterm"
    DeleteRegKey HKLM "Software\cterm"
SectionEnd
