"""Settings read from the environment once at import, changeable with glint.configure()."""

from __future__ import annotations

import os
from typing import Optional

DEFAULT_URL = "http://127.0.0.1:23600"
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


class Settings:
    def __init__(self) -> None:
        self.url: str = _env("GLINT_URL") or DEFAULT_URL
        self.enabled: bool = _enabled_from_env()
        self.project: Optional[str] = _env("GLINT_PROJECT") or None


settings = Settings()
