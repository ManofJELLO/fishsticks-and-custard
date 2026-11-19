// The popup's content, the same in both kinds of window: the drawn panel
// behind, then the status line, the visualiser, the conversation and the
// text field. Everything it shows comes from Backend; everything you do in
// it goes back there.
import QtQuick
import QtQuick.Controls.Basic
import QtQuick.Layouts
import QtQuick.Window
import dev.fsc.app

FocusScope {
    id: root

    readonly property int padX: 24
    // The text line whose highlight Ctrl+C copies (the one last selected in).
    property Item picked: null

    implicitWidth: Backend.popupWidth
    implicitHeight: content.implicitHeight + content.anchors.topMargin + content.anchors.bottomMargin

    function fg(a) { return Qt.rgba(Backend.fg.r, Backend.fg.g, Backend.fg.b, a) }
    function accentA(a) { return Qt.rgba(Backend.accent.r, Backend.accent.g, Backend.accent.b, a) }

    function focusEntry() { entry.forceActiveFocus() }

    // Keys the popup answers wherever focus is in it.
    function handleKey(event) {
        const ctrl = event.modifiers & Qt.ControlModifier
        const shift = event.modifiers & Qt.ShiftModifier
        if (event.key === Qt.Key_Escape) {
            Backend.close()
        } else if (ctrl && shift && event.key === Qt.Key_C) {
            // Ctrl+Shift+C: the whole conversation. Caps Lock's "C" without
            // Shift is still Ctrl+C.
            Backend.copyHistory()
        } else if (ctrl && event.key === Qt.Key_C && entry.selectedText.length === 0) {
            // Ctrl+C copies what's highlighted in the conversation; with
            // nothing highlighted anywhere it stops everything, like a terminal.
            if (root.picked && root.picked.selectedText.length > 0) {
                Backend.copyText(root.picked.selectedText)
            } else {
                Backend.stopAll()
            }
        } else if (ctrl && event.key === Qt.Key_N) {
            Backend.newConversation()
        } else {
            return
        }
        event.accepted = true
    }

    // A line in the conversation was highlighted in: it's the one Ctrl+C
    // copies, and the line before loses its highlight.
    function pick(item) {
        if (root.picked && root.picked !== item) {
            root.picked.deselect()
        }
        root.picked = item
    }

    Keys.onPressed: (event) => handleKey(event)

    // Where the header, visualiser and text sit, for the panel's gradient.
    function paintPanel() {
        if (width < 2 || height < 2) {
            return
        }
        const headerBottom = header.mapToItem(root, 0, header.height).y
        const vizTop = viz.mapToItem(root, 0, 0).y
        const vizBottom = viz.mapToItem(root, 0, viz.height).y
        const textItem = history.visible ? history : entry
        const textTop = textItem.mapToItem(root, 0, 0).y
        Backend.paintPanel(width, height, headerBottom, vizTop, vizBottom, textTop)
    }
    function paintPanelSoon() { Qt.callLater(root.paintPanel) }
    onWidthChanged: paintPanelSoon()
    onHeightChanged: paintPanelSoon()
    Component.onCompleted: paintPanelSoon()

    Connections {
        target: Backend
        function onRepaint(part) {
            if (part === "panel") panel.update()
            else if (part === "status") status.update()
            else if (part === "viz") viz.update()
        }
        function onBgChanged() { root.paintPanelSoon() }
        function onFgChanged() { root.paintPanelSoon() }
        function onHistoryShownChanged() { root.paintPanelSoon() }
        function onClearPick() {
            if (root.picked) root.picked.deselect()
            root.picked = null
        }
    }

    // The desktop's colours, followed live.
    SystemPalette {
        id: sys
        colorGroup: SystemPalette.Active
        function send() { Backend.setSystemColors(sys.window, sys.windowText, sys.accent) }
        onWindowChanged: send()
        onWindowTextChanged: send()
        onAccentChanged: send()
        Component.onCompleted: send()
    }

    // Animation runs only while the popup is up.
    FrameAnimation {
        running: root.Window.window ? root.Window.window.visible : false
        onTriggered: Backend.frame(frameTime, viz.width, viz.height, status.width, status.height)
    }

    // Drawn at the screen's pixel density, so it stays sharp when scaled.
    Binding {
        target: Backend
        property: "scale"
        value: Screen.devicePixelRatio
    }
    Connections {
        target: Backend
        function onScaleChanged() { root.paintPanelSoon() }
    }

    Raster {
        id: panel
        part: "panel"
        anchors.fill: parent
    }

    ColumnLayout {
        id: content
        anchors.fill: parent
        anchors.topMargin: 20
        anchors.bottomMargin: 20
        anchors.leftMargin: root.padX
        anchors.rightMargin: root.padX
        spacing: 16

        RowLayout {
            id: header
            Layout.fillWidth: true
            spacing: 12
            onHeightChanged: root.paintPanelSoon()

            Raster {
                id: status
                part: "status"
                Layout.fillWidth: true
                Layout.preferredHeight: Backend.statusHeight
            }
            Text {
                Layout.alignment: Qt.AlignVCenter
                text: Backend.meta
                color: root.fg(0.72)
                font.family: Backend.fontFamily
                font.pixelSize: 10
                font.letterSpacing: 2
                // A soft halo so it reads over the visualiser's glow.
                style: Text.Outline
                styleColor: Qt.rgba(Backend.bg.r, Backend.bg.g, Backend.bg.b, 0.5)
            }
        }

        Raster {
            id: viz
            part: "viz"
            Layout.fillWidth: true
            Layout.preferredHeight: Backend.vizHeight
            onYChanged: root.paintPanelSoon()
        }

        Flickable {
            id: history
            Layout.fillWidth: true
            Layout.preferredHeight: Math.min(blocks.implicitHeight, Backend.maxHeight)
            visible: Backend.historyShown
            clip: true
            contentWidth: width
            contentHeight: blocks.implicitHeight
            boundsBehavior: Flickable.StopAtBounds
            onYChanged: root.paintPanelSoon()

            // Follow new text only while you're at the end. Scrolled up to
            // read, the text stays still: nothing moves unless you move it.
            property bool stick: true
            property bool placing: false
            function atEnd() { return contentY >= contentHeight - height - 4 }
            function toEnd() {
                placing = true
                contentY = Math.max(0, contentHeight - height)
                placing = false
            }
            onContentYChanged: if (!placing) stick = atEnd()
            onContentHeightChanged: if (stick) toEnd()
            onHeightChanged: if (stick) toEnd()

            Connections {
                target: Backend
                function onScrollToEnd(force) {
                    if (force) history.stick = true
                    if (history.stick) history.toEnd()
                }
            }

            // Thin, and only there while scrolling or pointed at, like an
            // overlay scrollbar.
            ScrollBar.vertical: ScrollBar {
                id: bar
                policy: ScrollBar.AsNeeded
                width: 6
                padding: 0
                background: null
                contentItem: Rectangle {
                    implicitWidth: 3
                    radius: 99
                    color: root.fg(0.14)
                    opacity: bar.active && bar.size < 1.0 ? 1 : 0
                    Behavior on opacity { NumberAnimation { duration: 200 } }
                }
            }

            Column {
                id: blocks
                width: history.width

                Repeater {
                    model: ListModel { id: blockModel }
                    delegate: Block {
                        width: blocks.width
                        popup: root
                    }
                }
            }

            Connections {
                target: Backend
                function onHistoryInsert(index, json) { blockModel.insert(index, JSON.parse(json)) }
                function onHistoryUpdate(index, json) { blockModel.set(index, JSON.parse(json)) }
                function onHistoryRemove(index, count) { blockModel.remove(index, count) }
                function onHistoryClear() {
                    root.picked = null
                    blockModel.clear()
                    history.stick = true
                }
            }
        }

        TextField {
            id: entry
            Layout.fillWidth: true
            focus: true
            placeholderText: "Ask anything…"
            placeholderTextColor: root.fg(0.35)
            color: root.fg(0.95)
            selectionColor: root.accentA(0.35)
            selectedTextColor: root.fg(0.95)
            font.family: Backend.fontFamily
            font.pixelSize: 14
            leftPadding: 18
            rightPadding: 18
            topPadding: 8
            bottomPadding: 8
            implicitHeight: 26 + topPadding + bottomPadding
            onYChanged: root.paintPanelSoon()
            background: Rectangle {
                radius: height / 2
                color: root.fg(0.04)
                border.width: 1
                border.color: entry.activeFocus ? root.fg(0.55) : root.fg(0.22)
            }
            cursorDelegate: Rectangle {
                width: 1.5
                color: Backend.accent
                visible: entry.cursorVisible
                SequentialAnimation on opacity {
                    running: entry.cursorVisible
                    loops: Animation.Infinite
                    NumberAnimation { to: 1; duration: 0 }
                    PauseAnimation { duration: 530 }
                    NumberAnimation { to: 0; duration: 0 }
                    PauseAnimation { duration: 530 }
                }
            }
            Keys.onPressed: (event) => root.handleKey(event)
            onAccepted: {
                if (Backend.submit(text)) {
                    text = ""
                }
            }
            onTextChanged: Backend.setEntryEmpty(text.length === 0)
        }
    }
}
