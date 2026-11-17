"""Windows-only WSL selection shared by native launchers."""
import os
import re

DEFAULT_WSL_DISTRO = "Ubuntu"


def resolve_wsl_distro(environ=None):
    selected = (os.environ if environ is None else environ).get("ECHO_WSL_DISTRO") or DEFAULT_WSL_DISTRO
    if re.fullmatch(r"[A-Za-z0-9._-]+", selected) is None:
        raise ValueError("ECHO_WSL_DISTRO must contain only A-Za-z0-9._-")
    return selected
