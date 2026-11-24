; Spark 2 (Rust) needs the WebView2 Evergreen runtime. customInit runs in .onInit, before the
; previous version's uninstaller, so a machine without WebView2 keeps its working Electron install.
!macro customInit
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ReadRegStr $0 HKCU "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" "pv"
  ${EndIf}
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    IfSilent +2
      MessageBox MB_OK|MB_ICONSTOP "O Spark precisa do Microsoft Edge WebView2 Runtime, que não foi encontrado.$\r$\nInstale-o em https://go.microsoft.com/fwlink/p/?LinkId=2124703 e rode este instalador de novo.$\r$\nNada foi alterado."
    SetErrorLevel 2
    Quit
  ${EndIf}
!macroend
