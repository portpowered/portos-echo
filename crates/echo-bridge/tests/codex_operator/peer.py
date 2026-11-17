"""Controlled account process. Never connects to an issuer or reads real auth."""

import json
import os
import pathlib
import socket
import sys

home = pathlib.Path(os.environ["CODEX_HOME"])
assert "OPENAI_API_KEY" not in os.environ
assert "OPENAI_ACCESS_TOKEN" not in os.environ
control = socket.socket()
control.bind(("127.0.0.1", 0))
control.listen(1)
home.joinpath("peer-port").write_text(str(control.getsockname()[1]))
home.joinpath("peer-pid").write_text(str(os.getpid()))


def emit(value):
    print(json.dumps(value), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    home.joinpath("last-method").write_text(method)
    if method == "initialize":
        assert request["params"]["capabilities"]["experimentalApi"] is True
        emit({"id": request["id"], "result": {}})
    elif method == "initialized":
        pass
    elif method == "account/login/start":
        kind = request["params"]["type"]
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
        account = {"type": "apiKey"} if mode == "api_key" else {
            "type": "chatgpt", "email": "synthetic@example.invalid", "planType": "synthetic"}
        emit({"id": request["id"], "result": {"account": account, "requiresOpenaiAuth": True}})
    else:
        raise AssertionError("unexpected account request")
