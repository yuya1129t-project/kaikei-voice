; 更新時に、起動中のReazonSpeech常駐サーバーを先に終了してファイルロックを解除する
!macro NSIS_HOOK_PREINSTALL
  nsExec::Exec 'taskkill /F /IM "sherpa-onnx-offline-websocket-server.exe"'
!macroend
