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
#include <QtGui/QWindow>

#ifdef __APPLE__
#include <objc/message.h>
#include <objc/runtime.h>
#endif

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

#ifdef __APPLE__
namespace mac {
// Objective-C messages from C++, through the runtime (no .mm file needed).
inline id send(id obj, const char* sel)
{
    return reinterpret_cast<id (*)(id, SEL)>(objc_msgSend)(obj, sel_registerName(sel));
}
inline bool responds(id obj, const char* sel)
{
    return reinterpret_cast<BOOL (*)(id, SEL, SEL)>(objc_msgSend)(obj, sel_registerName("respondsToSelector:"), sel_registerName(sel));
}
} // namespace mac
#endif

// macOS: make the popup's window float like a launcher's. It shows on every
// Space and over full-screen apps, stays above other windows, and isn't
// hidden by macOS when another app becomes active (fscd keeps track of
// whether it's shown). With `activate`, fscd also becomes the active app,
// so typing goes to the popup. Nothing elsewhere.
inline void float_window(QObject* window, bool activate)
{
#ifdef __APPLE__
    auto* w = qobject_cast<QWindow*>(window);
    if (!w) {
        return;
    }
    // On macOS a window's id is its NSView (made now if need be).
    id view = reinterpret_cast<id>(w->winId());
    id nswindow = view ? mac::send(view, "window") : nullptr;
    if (nswindow) {
        // NSWindowCollectionBehaviorCanJoinAllSpaces | ...FullScreenAuxiliary
        const unsigned long behavior = (1UL << 0) | (1UL << 8);
        reinterpret_cast<void (*)(id, SEL, unsigned long)>(objc_msgSend)(
            nswindow, sel_registerName("setCollectionBehavior:"), behavior);
        // NSFloatingWindowLevel
        reinterpret_cast<void (*)(id, SEL, long)>(objc_msgSend)(nswindow, sel_registerName("setLevel:"), 3L);
        reinterpret_cast<void (*)(id, SEL, BOOL)>(objc_msgSend)(nswindow, sel_registerName("setHidesOnDeactivate:"), NO);
    }
    if (activate) {
        id app = mac::send(reinterpret_cast<id>(objc_getClass("NSApplication")), "sharedApplication");
        if (app && mac::responds(app, "activate")) {
            mac::send(app, "activate"); // macOS 14 and later
        } else if (app) {
            reinterpret_cast<void (*)(id, SEL, BOOL)>(objc_msgSend)(app, sel_registerName("activateIgnoringOtherApps:"), YES);
        }
    }
#else
    (void)window;
    (void)activate;
#endif
}

} // namespace fsc
