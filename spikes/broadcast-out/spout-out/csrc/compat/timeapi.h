// mingw-w64 10 has no <timeapi.h>; its timeBeginPeriod and friends live in <mmsystem.h>.
#pragma once
#include <windows.h>
#include <mmsystem.h>
