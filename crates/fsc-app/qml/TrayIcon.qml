// The tray icon and its menu when Qt shows them: in the macOS menu bar,
// where it's a native status item (and on Linux with FSC_QT_TRAY=1).
//
// The menu comes from Backend.trayMenu (JSON; see tray_qt.rs). It's built
// once, then kept the same length and order: entries are greyed out, never
// added or removed, so nothing moves under the pointer. Only a submenu's
// own list is redone when what it lists changes (a mic plugged in, the
// agent's models loaded).
import QtQuick
import Qt.labs.platform as P
import dev.fsc.app

P.SystemTrayIcon {
    id: tray

    visible: Backend.trayIcon !== ""
    icon.source: Backend.trayIcon
    icon.mask: Backend.trayMask
    tooltip: Backend.trayTip.length > 0 ? "Fishsticks & Custard · " + Backend.trayTip : "Fishsticks & Custard"

    menu: P.Menu {
        id: menu
        onAboutToShow: Backend.trayOpening()
    }

    // The top-level entries as built, in order.
    property var entries: []

    // (Properties, not children: a SystemTrayIcon takes none.)
    property Component itemPart: Component {
        P.MenuItem {
            id: entry
            // The index of its command in Rust (-1: none, a note).
            property int act: -1
            onTriggered: tray.choose(entry)
        }
    }
    property Component separatorPart: Component {
        P.MenuSeparator {}
    }
    property Component submenuPart: Component {
        P.Menu {
            // Its option entries, and what they were built for.
            property var options: []
            property string built: ""
        }
    }

    property Connections updates: Connections {
        target: Backend
        function onTrayMenuChanged() { tray.show() }
    }

    // Menus take "&" as a mnemonic marker; double it to show one.
    function label(s) { return s.replace(/&/g, "&&") }

    function choose(entry) {
        if (entry.act >= 0) Backend.trayActivate(entry.act)
        // A click toggles a checkable entry by itself; the menu shows the
        // state as Backend has it (an update follows if it changed).
        Qt.callLater(tray.show)
    }

    function show() {
        if (Backend.trayMenu.length === 0) return
        const items = JSON.parse(Backend.trayMenu)
        if (entries.length === 0) build(items)
        else update(items)
    }

    function build(items) {
        const made = []
        for (const it of items) {
            let e
            if (it.kind === "sep") {
                e = separatorPart.createObject(menu)
                menu.addItem(e)
            } else if (it.kind === "choice") {
                e = submenuPart.createObject(menu)
                fill(e, it)
                menu.addMenu(e)
            } else {
                e = itemPart.createObject(menu, { checkable: it.kind === "check" })
                set(e, it)
                menu.addItem(e)
            }
            made.push(e)
        }
        entries = made
    }

    function update(items) {
        for (let i = 0; i < items.length && i < entries.length; i++) {
            const it = items[i], e = entries[i]
            if (it.kind === "choice") fill(e, it)
            else if (it.kind !== "sep") set(e, it)
        }
    }

    // An action or a checkbox entry.
    function set(e, it) {
        e.text = label(it.label)
        e.act = it.act
        if (it.kind === "check") e.checked = it.checked
    }

    // A submenu: its title, and one radio entry per option. The entries
    // are rebuilt only when the options themselves changed.
    function fill(sub, it) {
        sub.title = label(it.label)
        sub.enabled = it.enabled
        const shape = JSON.stringify(it.options.map(o => [o.label, o.act]))
        if (shape !== sub.built) {
            for (const o of sub.options) {
                sub.removeItem(o)
                o.destroy()
            }
            const made = []
            for (const o of it.options) {
                const e = itemPart.createObject(sub, { checkable: o.act >= 0 })
                sub.addItem(e)
                made.push(e)
            }
            sub.options = made
            sub.built = shape
        }
        it.options.forEach((o, i) => {
            const e = sub.options[i]
            e.text = label(o.label)
            e.act = o.act
            e.enabled = o.enabled && o.act >= 0
            e.checked = i === it.selected
        })
    }

    Component.onCompleted: show()
}
