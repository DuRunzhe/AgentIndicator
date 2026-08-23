on run argv
    set targetTTY to item 1 of argv
    try
        if application "Terminal" is running then
            tell application "Terminal"
                repeat with windowIndex from 1 to count of windows
                    set terminalWindow to item windowIndex of windows
                    repeat with tabIndex from 1 to count of tabs of terminalWindow
                        set terminalTab to item tabIndex of tabs of terminalWindow
                        if tty of terminalTab is targetTTY then
                            set selected of terminalTab to true
                            set index of terminalWindow to 1
                            activate
                            return "Terminal"
                        end if
                    end repeat
                end repeat
            end tell
        end if
    end try
    try
        if application "iTerm2" is running then
            tell application "iTerm2"
                repeat with terminalWindow in windows
                    repeat with terminalTab in tabs of terminalWindow
                        repeat with terminalSession in sessions of terminalTab
                            if tty of terminalSession is targetTTY then
                                tell terminalSession to select
                                activate
                                return "iTerm2"
                            end if
                        end repeat
                    end repeat
                end repeat
            end tell
        end if
    end try
    return ""
end run
