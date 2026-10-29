Unicode true
ManifestDPIAware true
RequestExecutionLevel admin
Name "Typst Editor"
OutFile "${OUTPUT_EXE}"
InstallDir "$PROGRAMFILES64\Typst Editor"
BrandingText "Typst Editor"
SetCompressor /SOLID lzma
XPStyle on

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

!define MUI_ABORTWARNING
!define MUI_ICON "${APP_ICON}"
!define MUI_UNICON "${APP_ICON}"
!define MUI_BGCOLOR "E6E6E6"
!define MUI_TEXTCOLOR "3C3C3C"
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_RIGHT
!define MUI_HEADERIMAGE_BITMAP "${HEADER_BMP}"
!define MUI_WELCOMEFINISHPAGE_BITMAP "${WELCOME_BMP}"
!define MUI_WELCOMEPAGE_TITLE "$(AppWelcomeTitle)"
!define MUI_WELCOMEPAGE_TEXT "$(AppWelcomeText)"
!define MUI_FINISHPAGE_TITLE "$(AppFinishTitle)"
!define MUI_FINISHPAGE_TEXT "$(AppFinishText)"
!define MUI_FINISHPAGE_RUN "$INSTDIR\typst-editor.exe"
!define MUI_FINISHPAGE_RUN_TEXT "$(AppRunText)"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

LangString AppWelcomeTitle ${LANG_SIMPCHINESE} "安装 Typst Editor"
LangString AppWelcomeTitle ${LANG_ENGLISH} "Install Typst Editor"
LangString AppWelcomeText ${LANG_SIMPCHINESE} "Typst Editor 是一个专注长文档的 Typst 编辑器。$\r$\n$\r$\n版本 ${VERSION} · Windows x64$\r$\n$\r$\n安装器会通过 Windows Installer 写入程序文件、开始菜单快捷方式和标准卸载项。"
LangString AppWelcomeText ${LANG_ENGLISH} "Typst Editor is a Typst editor focused on long-form writing.$\r$\n$\r$\nVersion ${VERSION} · Windows x64$\r$\n$\r$\nThe installer uses Windows Installer for program files, the Start Menu shortcut, and the standard uninstall entry."
LangString AppInstalling ${LANG_SIMPCHINESE} "正在安装 Typst Editor..."
LangString AppInstalling ${LANG_ENGLISH} "Installing Typst Editor..."
LangString AppInstallFailed ${LANG_SIMPCHINESE} "安装失败。请查看日志获取详细信息。"
LangString AppInstallFailed ${LANG_ENGLISH} "Installation failed. See the log for details."
LangString AppFinishTitle ${LANG_SIMPCHINESE} "Typst Editor 安装完成"
LangString AppFinishTitle ${LANG_ENGLISH} "Typst Editor is installed"
LangString AppFinishText ${LANG_SIMPCHINESE} "Typst Editor 已安装到这台电脑。$\r$\n$\r$\n首次启动会进行系统字体扫描。"
LangString AppFinishText ${LANG_ENGLISH} "Typst Editor is installed on this computer.$\r$\n$\r$\nThe first launch scans the system fonts."
LangString AppRunText ${LANG_SIMPCHINESE} "运行 Typst Editor"
LangString AppRunText ${LANG_ENGLISH} "Run Typst Editor"

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Typst Editor"
VIAddVersionKey "CompanyName" "typ-edit"
VIAddVersionKey "FileDescription" "Typst Editor Installer"
VIAddVersionKey "LegalCopyright" "All rights reserved"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"

Function .onInit
    System::Call 'kernel32::GetUserDefaultUILanguage() i .r0'
    ${If} $0 == 0x0804
        StrCpy $LANGUAGE ${LANG_SIMPCHINESE}
    ${Else}
        StrCpy $LANGUAGE ${LANG_ENGLISH}
    ${EndIf}
FunctionEnd

Section "Typst Editor"
    SectionIn RO
    InitPluginsDir
    SetOutPath "$PLUGINSDIR"
    File /oname=TypstEditor.msi "${MSI_PATH}"

    DetailPrint "$(AppInstalling)"
    SetDetailsPrint both
    StrCpy $0 0
    ExecWait '"$SYSDIR\msiexec.exe" /i "$PLUGINSDIR\TypstEditor.msi" /qn /norestart /l*v "$TEMP\TypstEditor-install.log"' $0

    ${If} $0 == 3010
        SetRebootFlag true
    ${ElseIf} $0 == 1641
        SetRebootFlag true
    ${ElseIf} $0 != 0
        MessageBox MB_ICONSTOP|MB_OK "$(AppInstallFailed)$\r$\n$\r$\nExit code: $0$\r$\nLog: $TEMP\TypstEditor-install.log"
        Abort
    ${EndIf}

    ${If} ${RunningX64}
        StrCpy $INSTDIR "$PROGRAMFILES64\Typst Editor"
    ${Else}
        StrCpy $INSTDIR "$PROGRAMFILES\Typst Editor"
    ${EndIf}

    Delete "$PLUGINSDIR\TypstEditor.msi"
SectionEnd
