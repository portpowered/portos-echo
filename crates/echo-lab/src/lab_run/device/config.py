"""Offline projection of an explicitly nominated TOML; never reads credentials.

The caller validates both input and output with echo-config. The default path
adds the credential-free restrictions; explicit real mode only overlays voice.
"""
import json
from pathlib import Path
import sys
import tomllib
from urllib.parse import urlsplit


def local_socket(value, ephemeral=False):
    host, port = value.split(":")
    if host != "127.0.0.1" or not port.isascii() or not port.isdigit():
        raise ValueError("not local")
    if not (0 if ephemeral else 1) <= int(port) <= 65535:
        raise ValueError("invalid port")


def local_url(value):
    url = urlsplit(value)
    if url.scheme != "http" or url.username or url.password or url.fragment:
        raise ValueError("not local")
    local_socket(url.netloc)


def table_lines(table, prefix=""):
    lines = []
    for key, value in table.items():
        if not isinstance(value, dict) and not (
                isinstance(value, list) and value and isinstance(value[0], dict)):
            lines.append(json.dumps(key) + " = " + json.dumps(value, ensure_ascii=False))
    for key, value in table.items():
        name = prefix + json.dumps(key)
        if isinstance(value, dict):
            lines += ["[" + name + "]"] + table_lines(value, name + ".")
        elif isinstance(value, list) and value and isinstance(value[0], dict):
            for item in value:
                lines += ["[[" + name + "]]"] + table_lines(item, name + ".")
    return lines


def restrict(config):
    """The default lane keeps the existing credential-free projection."""
    if "muse" in config:
        raise ValueError("Muse credentials are outside the local M1 journey")
    local_socket(str(config["echo"]["host"]) + ":" + str(config["echo"].get("wyoming_port", 16700)))
    local_socket(config["screen"].get("listen", ""), True)
    local_socket(config["mcp"].get("operator_listen", "127.0.0.1:0"), True)
    if "home" in config.get("codex", {}):
        raise ValueError("real auth state is not admitted")
    if "state_dir" in config["mcp"]:
        raise ValueError("real auth state is not admitted")
    for server in config["mcp"].get("server", []):
        local_url(server["url"])
        if server["auth"] != "none" or "bearer_file" in server:
            raise ValueError("credentialed MCP is not admitted")
    config["live"].update(mode="api_key", voice="alloy", session_idle_seconds=0,
                          credential_file="credential")
    config["screen"].update(listen="127.0.0.1:0", pairing_token_file="pairing")
    config["audit"]["log_dir"] = "audit"


def project(path, settings=None, real=False):
    with path.open("rb") as source:
        config = tomllib.loads(source.read(65537).decode("utf-8"))
    for name in ["echo", "live", "agent", "mcp", "screen", "audit"]:
        if not isinstance(config[name], dict):
            raise ValueError("missing table")
    # Both modes inspect only this nominated TOML, never any referenced state.
    if not real:
        restrict(config)
    if settings:
        if any(key not in ("session_listen_seconds", "session_max_seconds")
               or type(value) is not int for key, value in settings.items()):
            raise ValueError("invalid voice setting")
        config.setdefault("voice", {}).update(settings)
    return "\n".join(table_lines(config)) + "\n"


try:
    settings = json.loads(sys.argv[2]) if len(sys.argv) > 2 else {}
    real = len(sys.argv) > 3 and sys.argv[3] == "real"
    print(json.dumps({"toml": project(Path(sys.argv[1]), settings, real)}))
except (OSError, ValueError, TypeError, KeyError, OverflowError):
    sys.stderr.write("device: invalid or unsafe nominated bridge configuration\n")
    sys.exit(1)
