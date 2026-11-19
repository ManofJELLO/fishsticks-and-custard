// The popup as a layer-shell surface (Wayland): an overlay at an edge or a
// corner of the screen, translucent, above every window. It only takes the
// keyboard when you click in it or it's opened to type.
import QtQuick
import QtQuick.Window
import org.kde.layershell 1.0 as LayerShell
import dev.fsc.app

Window {
    id: win

    title: "Fishsticks & Custard"
    color: "transparent"
    visible: false
    width: Backend.popupWidth
    height: popup.implicitHeight

    LayerShell.Window.scope: "fsc"
    LayerShell.Window.layer: LayerShell.Window.LayerOverlay
    // On demand, not exclusive: other windows stay usable while it's up.
    LayerShell.Window.keyboardInteractivity: LayerShell.Window.KeyboardInteractivityOnDemand
    LayerShell.Window.anchors: Backend.anchors
    // `fsc show` mustn't take the keyboard; `fsc toggle`/`type` ask for it.
    LayerShell.Window.activateOnShow: false

    Component.onCompleted: {
        Backend.layerMargins(LayerShell.Window)
        // On the monitor named in the config, else wherever the compositor
        // puts it (the focused one).
        let found = null
        for (const s of Qt.application.screens) {
            if (s.name === Backend.monitor) found = s
        }
        if (Backend.monitor !== "" && !found) {
            console.warn("fsc: monitor " + Backend.monitor + " not found; using the focused one")
        }
        if (found) {
            LayerShell.Window.wantsToBeOnActiveScreen = false
            win.screen = found
        } else {
            LayerShell.Window.wantsToBeOnActiveScreen = true
        }
    }

    Popup {
        id: popup
        anchors.fill: parent
        focus: true
    }

    onClosing: (close) => {
        close.accepted = false
        Backend.close()
    }

    Connections {
        target: Backend
        function onShowPopup(focus) {
            win.visible = true
            if (focus) {
                win.requestActivate()
                popup.focusEntry()
            }
        }
        function onHidePopup() { win.visible = false }
    }

    Shot { target: popup }
}
