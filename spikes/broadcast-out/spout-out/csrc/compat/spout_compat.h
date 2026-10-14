// Force-included into every Spout translation unit so the SDK builds with mingw-w64 10 (win32 thread model).
// The SDK itself is unmodified.
#pragma once
#include <chrono>

#ifndef _GLIBCXX_HAS_GTHREADS
#include <windows.h>
// The win32 thread model has no <thread>, and the SDK uses exactly one piece of it: sleep_for in HoldFps.
namespace std {
namespace this_thread {
template <class Rep, class Period>
inline void sleep_for(const chrono::duration<Rep, Period>& d) {
    Sleep(static_cast<DWORD>(chrono::duration_cast<chrono::milliseconds>(d).count()));
}
}  // namespace this_thread
}  // namespace std
#endif
