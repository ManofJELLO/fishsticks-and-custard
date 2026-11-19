// The few Qt calls the bridge needs that cxx-qt-lib doesn't bind.
#pragma once

#include <QtCore/QCoreApplication>
#include <QtCore/QMargins>
#include <QtCore/QObject>
#include <QtCore/QString>
#include <QtCore/QVariant>
#include <QtGui/QClipboard>
#include <QtGui/QGuiApplication>
#include <QtGui/QIcon>

namespace fsc {

inline void set_clipboard(const QString& text)
{
    if (QClipboard* c = QGuiApplication::clipboard()) {
        c->setText(text);
    }
}

// LayerShellQt's `margins` is a QMargins, which QML can't write. Set
// through the property system, so nothing links against LayerShellQt.
inline void set_margins(QObject* layer_window, int left, int top, int right, int bottom)
{
    if (layer_window) {
        layer_window->setProperty("margins", QVariant::fromValue(QMargins(left, top, right, bottom)));
    }
}

// Leave the event loop. Not quit(): in Qt 6 that first asks every window
// to close, and the popup refuses (closing it only hides it).
inline void quit_app()
{
    QCoreApplication::exit(0);
}

// The window icon, from the installed icon theme.
inline void set_icon_name(const QString& name)
{
    QGuiApplication::setWindowIcon(QIcon::fromTheme(name));
}

} // namespace fsc
