"""Controlled account process. Never connects to an issuer or reads real auth."""

import json
import os
import pathlib
import socket
import sys
import tomllib

home = pathlib.Path(os.environ["CODEX_HOME"])
overrides = {}
for index, argument in enumerate(sys.argv):
    if argument == "-c":
        key, value = sys.argv[index + 1].split("=", 1)
        assert key not in overrides
        overrides[key] = tomllib.loads("value=" + value)["value"]
routing_keys = ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
                "http_proxy", "https_proxy", "all_proxy", "no_proxy")
home.joinpath("routing.json").write_text(json.dumps({
    "config": overrides, "proxy": {key: os.environ[key] for key in routing_keys if key in os.environ}}))
assert "OPENAI_API_KEY" not in os.environ
assert "OPENAI_ACCESS_TOKEN" not in os.environ
control = socket.socket()
control.bind(("127.0.0.1", 0))
control.listen(1)
home.joinpath("peer-port").write_text(str(control.getsockname()[1]))
home.joinpath("peer-pid").write_text(str(os.getpid()))
with home.joinpath("launches").open("a") as launches:
    launches.write("started\n")


def emit(value):
    print(json.dumps(value), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    home.joinpath("last-method").write_text(method)
    with home.joinpath("methods").open("a") as methods:
        methods.write(method + "\n")
    if method == "initialize":
        assert request["params"]["capabilities"]["experimentalApi"] is True
        if home.joinpath("mode").read_text() == "blocked-initialize":
            connection, _ = control.accept()
            with connection:
                connection.recv(64)
        emit({"id": request["id"], "result": {}})
    elif method == "initialized":
        pass
    elif method == "account/login/start":
        kind = request["params"]["type"]
        if kind == "apiKey":
            assert request["params"]["apiKey"] == "synthetic-nominated-key"
            emit({"id": request["id"], "result": {"type": "apiKey"}})
            continue
        assert kind in ("chatgptDeviceCode", "chatgpt")
        result = {"type": kind, "loginId": "local-login"}
        if kind == "chatgptDeviceCode":
            result.update(verificationUrl="http://127.0.0.1:12345/verify", userCode="TEST-CODE")
        else:
            result.update(authUrl="http://127.0.0.1:12345/authorize")
        emit({"id": request["id"], "result": result})
        emit({"method": "account/login/completed", "params": {
            "loginId": "foreign-login", "success": True}})
        connection, _ = control.accept()
        with connection:
            outcome = connection.recv(64)
        if outcome == b"eof":
            break
        success = outcome == b"success"
        if success:
            home.joinpath("auth.json").write_bytes(b'{"synthetic":"local-only"}\n')
        emit({"method": "account/login/completed", "params": {
            "loginId": "local-login", "success": success, "error": "synthetic private error"}})
    elif method == "account/read":
        assert request["params"] == {"refreshToken": False}
        mode = home.joinpath("mode").read_text()
        if mode == "blocked-account":
            connection, _ = control.accept()
            with connection:
                connection.recv(64)
        if mode in ("rejected-account", "expired-account"):
            emit({"id": request["id"], "error": {
                "code": -32000, "message": "synthetic private error",
                "data": {"reason": "invalid_grant" if mode == "expired-account" else "invalid_token",
                         "detail": "synthetic-provider-private-detail"}}})
            continue
        account = {"type": "apiKey"} if mode == "api_key" else {
            "type": "chatgpt", "email": "synthetic@example.invalid", "planType": "synthetic"}
        if mode == "signed-out":
            account = None
        emit({"id": request["id"], "result": {"account": account, "requiresOpenaiAuth": True}})
    else:
        raise AssertionError("unexpected account request")
