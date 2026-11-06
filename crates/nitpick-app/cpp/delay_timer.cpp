#include "nitpick-app/cpp/delay_timer.h"

#include "nitpick-app/src/delay.cxx.h"

#include <QtCore/QCoreApplication>
#include <QtCore/QTimer>

#include <chrono>

namespace nitpick {

void startDelayTimer(std::uint64_t timer, std::int32_t ms) {
    // A coarse timer can fire up to 5% early; the delay is a minimum.
    QTimer::singleShot(std::chrono::milliseconds(ms), Qt::PreciseTimer,
                       QCoreApplication::instance(), [timer] { delayElapsed(timer); });
}

} // namespace nitpick
