use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    let module = QmlModule::new("dev.fsc.app").qml_files([
        "qml/Block.qml",
        "qml/LayerPopup.qml",
        "qml/Line.qml",
        "qml/PillButton.qml",
        "qml/Popup.qml",
        "qml/Shot.qml",
        "qml/SpeakerButton.qml",
        "qml/WindowPopup.qml",
    ]);
    CxxQtBuilder::new_qml_module(module)
        .qt_module("Quick")
        .qt_module("QuickControls2")
        .files(["src/qt/bridge.rs"])
        .build();
}
