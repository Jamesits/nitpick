#include "nitpick-app/cpp/test_host.h"

#include "nitpick-app/src/test_host.cxx.h"

#include <QtCore/QCoreApplication>
#include <QtCore/QMetaObject>
#include <QtCore/qtenvironmentvariables.h>

namespace nitpick {

bool postToGuiThread(std::uint64_t job) {
    // Callable from any thread: the queued call runs in the thread of the
    // application object.
    QCoreApplication *app = QCoreApplication::instance();
    if (!app) {
        return false;
    }
    return QMetaObject::invokeMethod(app, [job] { runPostedJob(job); }, Qt::QueuedConnection);
}

void quitApplication(std::int32_t code) { QCoreApplication::exit(code); }

void useTimerUpdateRequests() {
#if defined(Q_OS_WIN)
    // qputenv updates the C runtime copy that Qt reads; SetEnvironmentVariable
    // of the Rust standard library does not.
    if (!qEnvironmentVariableIsSet("QT_D3D_NO_VBLANK_THREAD")) {
        qputenv("QT_D3D_NO_VBLANK_THREAD", "1");
    }
#endif
}

} // namespace nitpick
