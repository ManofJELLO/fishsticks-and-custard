// Development aid: with FSC_SHOT=/path/to.png set, the popup is saved there
// every second while it's up, so its look can be checked without taking
// focus or touching the screen (e.g. with QT_QPA_PLATFORM=offscreen).
import QtQuick
import dev.fsc.app

Timer {
    property Item target
    interval: 1000
    repeat: true
    running: Backend.shot !== "" && Backend.shown
    onTriggered: target.grabToImage((r) => r.saveToFile(Backend.shot))
}
