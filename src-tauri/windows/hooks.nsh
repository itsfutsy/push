; Adds Push to File Explorer's "Send to" menu: sending files there uploads them.
!macro NSIS_HOOK_POSTINSTALL
  CreateShortcut "$SENDTO\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete "$SENDTO\${PRODUCTNAME}.lnk"
!macroend
