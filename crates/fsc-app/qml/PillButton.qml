// A permission card's answer: a pill outline, or filled with the accent
// for the main one (ALLOW). Never takes focus from the text field.
import QtQuick
import QtQuick.Controls.Basic
import dev.fsc.app

Button {
    id: button

    property bool primary: false

    focusPolicy: Qt.NoFocus
    leftPadding: 16
    rightPadding: 16
    topPadding: 2
    bottomPadding: 2
    implicitHeight: Math.max(24, contentItem.implicitHeight + topPadding + bottomPadding)

    function fg(a) { return Qt.rgba(Backend.fg.r, Backend.fg.g, Backend.fg.b, a) }

    contentItem: Text {
        text: button.text
        font.family: Backend.fontFamily
        font.pixelSize: 11
        font.letterSpacing: 2
        color: button.primary ? Backend.accentText : button.fg(0.95)
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
    }

    background: Rectangle {
        radius: height / 2
        color: button.primary
               ? Qt.rgba(Backend.accent.r, Backend.accent.g, Backend.accent.b, button.hovered ? 0.85 : 1.0)
               : (button.hovered ? button.fg(0.04) : "transparent")
        border.width: 1
        border.color: button.primary ? Backend.accent : (button.hovered ? button.fg(0.55) : button.fg(0.22))
    }
}
