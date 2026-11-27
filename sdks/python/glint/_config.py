"""Settings read from the environment once at import, changeable with glint.configure()."""

from __future__ import annotations

import os
from typing import Optional

DEFAULT_URL = "http://127.0.0.1:23600"
# Inside a container 127.0.0.1 is the container itself; Docker Desktop (and `host-gateway` on
# Linux) names the host machine host.docker.internal.
CONTAINER_URL = "http://host.docker.internal:23600"
_PRODUCTION_VARS = ("APP_ENV", "ENV", "PYTHON_ENV", "DJANGO_ENV")


def _env(name: str) -> Optional[str]:
    try:
        return os.environ.get(name)
    except Exception:
        return None


def _enabled_from_env() -> bool:
    flag = (_env("GLINT_ENABLED") or "").strip().lower()
    if flag in ("0", "false", "off"):
        return False
    if flag in ("1", "true", "on"):
        return True
    # Default: off in production, on everywhere else.
    return not any((_env(name) or "").strip().lower() == "production" for name in _PRODUCTION_VARS)


def _in_container() -> bool:
    try:
        if os.path.exists("/.dockerenv") or os.path.exists("/run/.containerenv"):
            return True
        with open("/proc/1/cgroup", encoding="utf-8", errors="replace") as f:
            text = f.read()
        return any(k in text for k in ("docker", "kubepods", "containerd", "libpod"))
    except Exception:
        return False


class Settings:
    def __init__(self) -> None:
        self.url: str = _env("GLINT_URL") or (CONTAINER_URL if _in_container() else DEFAULT_URL)
        self.enabled: bool = _enabled_from_env()
        self.project: Optional[str] = _env("GLINT_PROJECT") or None
        # "container:host" pairs, comma separated; glintd rewrites file paths with it.
        self.path_map: Optional[str] = _env("GLINT_PATH_MAP") or None


settings = Settings()
