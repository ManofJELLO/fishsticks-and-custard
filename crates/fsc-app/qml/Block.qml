// One line of the conversation: a prompt, the reply, a tool used, a note,
// an error, or a permission card with its buttons. Its role values come
// from Backend's history (see history.rs: key, group, kind, text, shown,
// gap, and the card's askId, action, detail, buttons, answered, note).
import QtQuick
import QtQuick.Layouts
import dev.fsc.app

Item {
    id: block

    required property var model
    // The popup, for its colours and the copy/highlight rules.
    property Item popup

    visible: model.shown
    implicitHeight: visible ? loader.implicitHeight + model.gap : 0

    readonly property string kind: model.kind

    function fg(a) { return popup.fg(a) }

    Loader {
        id: loader
        y: block.model.gap
        width: block.width
        sourceComponent: block.kind === "ask" ? cardPart
                       : (block.kind === "prompt" || block.kind === "reply") ? selectablePart
                       : plainPart
    }

    // Room a reply keeps at its right for its speaker, from its first word
    // on, so the speaker turning up at the end moves no text.
    readonly property int speakerRoom: 26

    // A prompt or a reply: its text can be highlighted for copying. It
    // never takes focus from the text field. A finished reply ends with a
    // speaker that reads it aloud (and, while it does, stops it).
    Component {
        id: selectablePart
        Item {
            implicitHeight: line.implicitHeight
            Line {
                id: line
                width: parent.width - (block.kind === "reply" ? block.speakerRoom : 0)
                text: block.model.text
                popup: block.popup
                color: block.kind === "reply" ? block.fg(0.95) : block.fg(0.55)
                font.pixelSize: block.kind === "reply" ? 15 : 13
            }
            SpeakerButton {
                id: speaker
                visible: block.kind === "reply" && block.model.finished
                reading: block.model.reading
                // Just after the last word: on its line, in the room kept.
                // Worked out once the text has been laid out.
                property rect end
                function place() {
                    end = line.positionToRectangle(line.text.replace(/\s+$/, "").length)
                }
                function placeSoon() { Qt.callLater(place) }
                Connections {
                    target: line
                    function onTextChanged() { speaker.placeSoon() }
                    function onContentHeightChanged() { speaker.placeSoon() }
                    function onContentWidthChanged() { speaker.placeSoon() }
                    function onWidthChanged() { speaker.placeSoon() }
                }
                onVisibleChanged: placeSoon()
                Component.onCompleted: placeSoon()
                x: Math.min(end.x + 6, parent.width - width)
                y: Math.max(0, Math.min(end.y + (end.height - height) / 2, parent.height - height))
                onClicked: Backend.readAloud(block.model.key)
            }
        }
    }

    Component {
        id: plainPart
        Text {
            text: block.model.text
            wrapMode: Text.WrapAtWordBoundaryOrAnywhere
            textFormat: Text.PlainText
            font.family: Backend.fontFamily
            color: block.kind === "error" ? Backend.error : block.fg(0.55)
            font.pixelSize: block.kind === "error" ? 12 : 11
            font.letterSpacing: block.kind === "error" ? 0 : 1
        }
    }

    // A permission question: tool, what it wants to do, the detail, and
    // the answers. Once answered, the buttons give way to what became of it.
    Component {
        id: cardPart
        Item {
            implicitHeight: card.implicitHeight + 4
            Rectangle {
                width: 2
                height: parent.height
                color: block.model.answered ? block.fg(0.14) : Backend.accent
            }
            ColumnLayout {
                id: card
                x: 14
                y: 2
                width: parent.width - 14
                spacing: 6

                Text {
                    Layout.fillWidth: true
                    text: block.model.text
                    wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                    textFormat: Text.PlainText
                    font.family: Backend.fontFamily
                    color: block.fg(0.55)
                    font.pixelSize: 11
                    font.letterSpacing: 1
                }
                Text {
                    Layout.fillWidth: true
                    text: block.model.action
                    wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                    textFormat: Text.PlainText
                    font.family: Backend.fontFamily
                    color: block.fg(0.95)
                    font.pixelSize: 14
                }
                Rectangle {
                    Layout.fillWidth: true
                    visible: block.model.detail.length > 0
                    implicitHeight: detail.implicitHeight + 12
                    radius: 8
                    color: block.fg(0.04)
                    Line {
                        id: detail
                        x: 10
                        y: 6
                        width: parent.width - 20
                        text: block.model.detail
                        popup: block.popup
                        color: block.fg(0.55)
                        font.pixelSize: 12
                    }
                }
                Row {
                    visible: block.model.buttons.length > 0
                    spacing: 8
                    Repeater {
                        model: block.model.buttons.length > 0 ? block.model.buttons.split(",") : []
                        delegate: PillButton {
                            required property string modelData
                            text: modelData
                            primary: modelData === "ALLOW"
                            onClicked: Backend.answer(block.model.askId, modelData)
                        }
                    }
                }
                Text {
                    visible: block.model.answered
                    Layout.fillWidth: true
                    text: block.model.note
                    wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                    font.family: Backend.fontFamily
                    color: block.fg(0.55)
                    font.pixelSize: 11
                    font.letterSpacing: 1
                }
            }
        }
    }
}
