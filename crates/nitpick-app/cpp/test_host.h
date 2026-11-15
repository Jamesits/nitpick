#pragma once

#include <cstdint>

namespace nitpick {

bool postToGuiThread(std::uint64_t job);
void quitApplication(std::int32_t code);
void useTimerUpdateRequests();

} // namespace nitpick
