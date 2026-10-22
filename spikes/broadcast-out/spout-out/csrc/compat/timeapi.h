// mingw-w64 10 has no <timeapi.h>; its timeBeginPeriod and friends live in <mmsystem.h>. Newer mingw-w64 (11 and up,
// as on Ubuntu 24.04) has the real header, which <mmsystem.h> itself includes: use it, or this shim would hide it.
#pragma once
#if __has_include_next(<timeapi.h>)
#include_next <timeapi.h>
#else
#include <windows.h>
#include <mmsystem.h>
#endif
