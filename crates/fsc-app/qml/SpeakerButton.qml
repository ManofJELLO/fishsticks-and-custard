// The speaker at the end of a reply: reads that reply aloud, and while it
// does, shows a stop square that ends it. Greyed out with no voice loaded.
// Hovering only lightens it; it never moves or takes focus.
import QtQuick
import QtQuick.Controls.Basic
import dev.fsc.app

Button {
    id: button

    property bool reading: false

    focusPolicy: Qt.NoFocus
    enabled: Backend.canSpeak
    padding: 0
    implicitWidth: 20
    implicitHeight: 18
    Accessible.name: reading ? "Stop reading this reply" : "Read this reply aloud"

    function fg(a) { return Qt.rgba(Backend.fg.r, Backend.fg.g, Backend.fg.b, a) }

    contentItem: Text {
        // Nerd Font glyphs: volume-high, and stop.
        text: String.fromCodePoint(button.reading ? 0xF04DB : 0xF057E)
        font.family: Backend.fontFamily
        font.pixelSize: 16
        color: !button.enabled ? button.fg(0.14)
             : button.reading ? Backend.accent
             : button.fg(button.hovered ? 0.95 : 0.5)
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }

    background: Rectangle {
        radius: 4
        color: button.hovered && button.enabled ? button.fg(0.06) : "transparent"
    }
}
