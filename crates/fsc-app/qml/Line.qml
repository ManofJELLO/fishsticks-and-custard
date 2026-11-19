// Text in the conversation that can be highlighted for copying, without
// ever taking keyboard focus from the text field.
import QtQuick
import dev.fsc.app

TextEdit {
    id: line

    property Item popup

    readOnly: true
    selectByMouse: true
    activeFocusOnPress: false
    // It never has focus, so its highlight must outlast not having it.
    persistentSelection: true
    wrapMode: TextEdit.WrapAtWordBoundaryOrAnywhere
    textFormat: TextEdit.PlainText
    font.family: Backend.fontFamily
    selectionColor: Qt.rgba(Backend.accent.r, Backend.accent.g, Backend.accent.b, 0.35)
    selectedTextColor: Qt.rgba(Backend.fg.r, Backend.fg.g, Backend.fg.b, 0.95)

    onSelectedTextChanged: if (selectedText.length > 0 && popup) popup.pick(line)
}
