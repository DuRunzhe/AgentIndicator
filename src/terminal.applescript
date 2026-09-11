property statusSeparator : character id 29
property recordSeparator : character id 30
property fieldSeparator : character id 31

on run targetTtys
    if application "Terminal" is not running then return "not_running" & statusSeparator
    tell application "Terminal"
        set outputText to ""
        repeat with terminalWindow in windows
            repeat with tabIndex from 1 to count of tabs of terminalWindow
                -- Read properties through a concrete tab specifier: contents of a
                -- repeat-variable reference dereferences the tab itself, not its text.
                set tabTty to (get tty of tab tabIndex of terminalWindow) as text
                if targetTtys contains tabTty then
                    set tabContents to (get contents of tab tabIndex of terminalWindow) as text
                    set outputText to outputText & tabTty & my fieldSeparator & tabContents & my recordSeparator
                end if
            end repeat
        end repeat
        return "running" & my statusSeparator & outputText
    end tell
end run

