Output demo.gif

Set Shell bash
Set FontFamily "JetBrains Mono"
Set FontSize 19
Set LineHeight 1.3
Set Width 1200
Set Height 800
Set Padding 28
Set Margin 40
Set MarginFill "#11111b"
Set BorderRadius 14
Set WindowBar Colorful
Set Theme "Catppuccin Mocha"
Set TypingSpeed 35ms
Set CursorBlink false

Hide
Type "podman exec -it -u alice -w /home/alice mix-demo bash --noprofile --rcfile /home/alice/.demo-rc" Enter
Wait+Line@30s /❯\s*$/
Type "clear" Enter
Wait+Line@30s /❯\s*$/
Show

Hide
Type `say "install a package"` Enter
Wait+Line@30s /❯\s*$/
Show
Sleep 1s
Type "mix install jq" Enter
Wait+Line@180s /❯\s*$/
Sleep 2s
Type "jq --version" Enter
Wait+Line@30s /❯\s*$/
Sleep 1s
Type "mix list" Enter
Wait+Line@60s /❯\s*$/
Sleep 2500ms

Hide
Type "clear" Enter
Wait+Line@30s /❯\s*$/
Type `say "preview a change before making it"` Enter
Wait+Line@30s /❯\s*$/
Show
Sleep 1s
Type "mix install fd --dry-run" Enter
Wait+Line@120s /❯\s*$/
Sleep 3500ms

Hide
Type "clear" Enter
Wait+Line@30s /❯\s*$/
Type `say "edit a managed file by hand, and mix notices"` Enter
Wait+Line@30s /❯\s*$/
Show
Sleep 1s
Type `echo "# oops" >> ~/.local/state/mix/home.nix` Enter
Wait+Line@30s /❯\s*$/
Sleep 1s
Type "mix doctor" Enter
Wait+Line@60s /❯\s*$/
Sleep 3500ms
Hide
Type "clear" Enter
Wait+Line@30s /❯\s*$/
Show
Type "mix repair" Enter
Wait+Line@60s /❯\s*$/
Sleep 1500ms
Type "mix doctor" Enter
Wait+Line@60s /❯\s*$/
Sleep 2s

Hide
Type "clear" Enter
Wait+Line@30s /❯\s*$/
Type `say "interrupt an install halfway"` Enter
Wait+Line@30s /❯\s*$/
Show
Sleep 1s
Type "mix install ffmpeg" Enter
Wait+Screen@120s /Installing ffmpeg/
Sleep 1s
Ctrl+C
Wait+Line@120s /❯\s*$/
Sleep 3500ms
Type "mix list" Enter
Wait+Line@60s /❯\s*$/
Sleep 3s
