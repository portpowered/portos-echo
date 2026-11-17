"""Offline projection of an explicitly nominated TOML; never reads credentials.

The caller validates both input and output with echo-config. This helper adds
the local, credential-free restrictions used by the device test journey.
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


def project(path):
    with path.open("rb") as source:
        config = tomllib.loads(source.read(65537).decode("utf-8"))
    for name in ["echo", "live", "agent", "mcp", "screen", "audit"]:
        if not isinstance(config[name], dict):
            raise ValueError("missing table")
    if "muse" in config:
        raise ValueError("Muse credentials are outside the local M1 journey")
    local_socket(str(config["echo"]["host"]) + ":" + str(config["echo"].get("wyoming_port", 16700)))
    local_socket(config["screen"].get("listen", ""), True)
    local_socket(config["mcp"].get("operator_listen", "127.0.0.1:0"), True)
    if "state_dir" in config["mcp"]:
        raise ValueError("real auth state is not admitted")
    for server in config["mcp"].get("server", []):
        local_url(server["url"])
        if server["auth"] != "none" or "bearer_file" in server:
            raise ValueError("credentialed MCP is not admitted")
    # Never resolve or read an input credential, even when the input selects
    # ChatGPT. Only the private copy selects the existing synthetic API mode.
    config["live"].update(mode="api_key", voice="alloy", session_idle_seconds=0,
                          credential_file="credential")
    config["screen"].update(listen="127.0.0.1:0", pairing_token_file="pairing")
    config["audit"]["log_dir"] = "audit"
    return "\n".join(table_lines(config)) + "\n"


try:
    print(json.dumps({"toml": project(Path(sys.argv[1]))}))
except (OSError, ValueError, TypeError, KeyError, OverflowError):
    sys.stderr.write("device: invalid or unsafe nominated bridge configuration\n")
    sys.exit(1)
