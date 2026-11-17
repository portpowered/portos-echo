"""Controlled stdio account peer; synthetic state only, no network or credentials."""

import json
import os
import pathlib
import sys


def emit(value):
    print(json.dumps(value), flush=True)


home = pathlib.Path(os.environ["CODEX_HOME"])
for line in sys.stdin:
    request = json.loads(line)
    method = request["method"]
    if method == "initialize":
        assert request["params"]["capabilities"]["experimentalApi"] is True
        emit({"id": request["id"], "result": {"userAgent": "synthetic-account-peer"}})
    elif method == "initialized":
        pass
    elif method == "account/login/start":
        assert request["params"] == {"type": "chatgptDeviceCode"}
        emit({"id": request["id"], "result": {
            "type": "chatgptDeviceCode", "loginId": "synthetic-login",
            "verificationUrl": "http://127.0.0.1:12345/verify", "userCode": "TEST-CODE"}})
        home.joinpath("auth.json").write_bytes(b'{"synthetic":"local-only"}\n')
        emit({"method": "account/login/completed", "params": {
            "loginId": "synthetic-login", "success": True}})
    elif method == "account/read":
        assert request["params"] == {"refreshToken": False}
        assert home.joinpath("auth.json").read_bytes() == b'{"synthetic":"local-only"}\n'
        emit({"id": request["id"], "result": {
            "account": {"type": "chatgpt", "planType": "synthetic"},
            "requiresOpenaiAuth": True}})
    else:
        raise AssertionError("unexpected account method")
