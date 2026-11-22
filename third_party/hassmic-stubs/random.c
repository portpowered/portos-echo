/* SPDX-License-Identifier: MIT */
/* The arbitration object references this helper. The WebSocket transport is absent. */
#include "netio.h"
#include "ws.h"
#include <fcntl.h>
#include <stdlib.h>

void ws_random(void *out, size_t len)
{
    int fd = open("/dev/urandom", O_RDONLY | O_CLOEXEC);
    if (fd < 0 || read_full(fd, out, len) != (ssize_t)len) abort();
    close(fd);
}
