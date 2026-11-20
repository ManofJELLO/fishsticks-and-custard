// The popup as a normal window: Hyprland's window rules (matched by the
// class dev.fsc.FishsticksAndCustard) float, place and decorate it. Also
// the root in headless mode, where it's never shown.
//
// On macOS it places itself, centred near the top of the screen, and floats
// like a launcher: on every Space and over full-screen apps.
import QtQuick
import QtQuick.Window
import dev.fsc.app

Window {
    id: win

    title: "Fishsticks & Custard"
    flags: Qt.Window | Qt.FramelessWindowHint
    color: "transparent"
    visible: false
    width: Backend.popupWidth
    height: popup.implicitHeight
    // A fixed size, as GTK's non-resizable window had: Hyprland floats a
    // window whose minimum and maximum sizes match, with no rule needed.
    // It still grows and shrinks with the conversation.
    minimumWidth: Backend.popupWidth
    maximumWidth: Backend.popupWidth
    minimumHeight: popup.implicitHeight
    maximumHeight: popup.implicitHeight

    readonly property bool mac: Qt.platform.os === "osx" || Qt.platform.os === "macos"

    Popup {
        id: popup
        anchors.fill: parent
        focus: true
    }

    // Closing it (e.g. Hyprland's killactive) hides it; the assistant keeps
    // running in the tray.
    onClosing: (close) => {
        close.accepted = false
        Backend.close()
    }

    Connections {
        target: Backend
        function onShowPopup(focus) {
            if (win.mac) {
                // Placed only as it opens: once up, it never moves.
                if (!win.visible) {
                    const s = win.screen
                    win.x = s.virtualX + Math.round((s.width - win.width) / 2)
                    win.y = s.virtualY + Math.round(s.height * 0.12)
                }
                Backend.floatWindow(win, focus)
            }
            win.visible = true
            win.raise()
            if (focus) {
                win.requestActivate()
                popup.focusEntry()
            }
        }
        function onHidePopup() { win.visible = false }
    }

    Shot { target: popup }
}
