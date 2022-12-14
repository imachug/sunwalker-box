"""
description: Pseudoterminals work and are reset correctly
runs: 2
expect:
  matching_stderr: true
"""

import os
import pty
import sys


os.close(1)
assert os.open("/dev/null", os.O_WRONLY) == 1


result = b""


def read(fd):
    global result
    result += os.read(fd, 1024)
    return result


# Make sure IDs are reset
pty.openpty()
print(os.listdir("/dev/pts"), file=sys.stderr)


pty.spawn(["/usr/bin/echo", "Hello, world!"], read)

assert result == b"Hello, world!\r\n", result
