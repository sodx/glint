"""ULID generation (https://github.com/ulid/spec), stdlib only."""

from __future__ import annotations

import os
import time

_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"


def ulid() -> str:
    """26 characters: 48-bit millisecond timestamp, then 80 random bits."""
    ms = int(time.time() * 1000) & ((1 << 48) - 1)
    value = (ms << 80) | int.from_bytes(os.urandom(10), "big")
    return "".join(_ALPHABET[(value >> (5 * i)) & 31] for i in range(25, -1, -1))
