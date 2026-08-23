property statusSeparator : character id 29
property recordSeparator : character id 30
property fieldSeparator : character id 31

on run targetTtys
    if application "Terminal" is not running then return "not_running" & statusSeparator
    tell application "Terminal"
        set outputText to ""
        repeat with terminalWindow in windows
            repeat with terminalTab in tabs of terminalWindow
                set tabTty to (get tty of terminalTab) as text
                if targetTtys contains tabTty then
                    set tabContents to (get contents of terminalTab) as text
                    set outputText to outputText & tabTty & my fieldSeparator & tabContents & my recordSeparator
                end if
            end repeat
        end repeat
        return "running" & my statusSeparator & outputText
    end tell
end run

