#ifndef SAPFLUX_FREESTANDING_STRING_H
#define SAPFLUX_FREESTANDING_STRING_H

#include <stddef.h>

/* The implementation is supplied by tinyrlibc. This header lets its small
 * C-only vsnprintf shim build with a freestanding ARM GCC toolchain. */
size_t strlen(const char *s);

#endif
