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
                "http_proxy", "https_proxy", "all_proxy", "no_proxy",
                "CODEX_REFRESH_TOKEN_URL_OVERRIDE")
home.joinpath("routing.json").write_text(json.dumps({
    "config": overrides, "proxy": {key: os.environ[key] for key in routing_keys if key in os.environ}}))
assert "OPENAI_API_KEY" not in os.environ
assert "OPENAI_ACCESS_TOKEN" not in os.environ
control = socket.socket()
control.bind(("127.0.0.1", 0))
control.listen(1)
# Kept for the entire child lifetime, never used by a protocol handler. The
# test holds the other end; EOF identifies this child rather than a reusable
# listening address. The supervisor also joins/reaps before releasing home.
lifetime = socket.create_connection(("127.0.0.1", int(home.joinpath("lifetime-port").read_text())))
lifetime.sendall(b"O")
assert lifetime.recv(1) == b"A"
home.joinpath("peer-port").write_text(str(control.getsockname()[1]))
home.joinpath("peer-pid").write_text(str(os.getpid()))
with home.joinpath("launches").open("a") as launches:
    launches.write("started\n")


def emit(value):
    print(json.dumps(value), flush=True)


for line in sys.stdin:
    request = json.loads(line)
    if "method" not in request:
        assert request["id"] == "fixture-tool"
        home.joinpath("tool-result").write_text(json.dumps(request["result"]))
        continue
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
    elif method == "thread/start":
        assert home.joinpath("mode").read_text().startswith("tool-")
        expected = ["extend_session", "end_session"] if home.joinpath("mode").read_text() == "tool-binary-reply" else ["test_echo", "test_wait", "test_requires_approval", "ui_show_emotion", "extend_session", "end_session"]
        assert [tool["name"] for tool in request["params"]["dynamicTools"]] == expected
        home.joinpath("thread-config").write_text(json.dumps(request["params"]))
        emit({"id": request["id"], "result": {"thread": {"id": "fixture-thread"}}})
    elif method == "thread/realtime/start":
        binary_reply = home.joinpath("mode").read_text() == "tool-binary-reply"
        if binary_reply:
            connection, _ = control.accept()
            signaling = connection.makefile("rwb", buffering=0)
            signaling.write((json.dumps(request["params"]["transport"]) + "\n").encode())
            answer = json.loads(signaling.readline())["answer"]
        else:
            assert request["params"]["transport"] == {"type": "webrtc", "sdp": "fixture-offer"}
            answer = "fixture-answer"
        assert request["params"]["version"] == "v3"
        home.joinpath("realtime-config").write_text(json.dumps(request["params"]))
        emit({"method": "thread/realtime/sdp", "params": {"threadId": "fixture-thread", "sdp": answer}})
        emit({"id": request["id"], "result": {}})
        delegated = binary_reply and home.joinpath("delegated").exists()
        if delegated:
            emit({"method": "turn/started", "params": {"threadId": "fixture-thread", "turn": {"id": "fixture-turn"}}})
        if binary_reply:
            assert signaling.readline() == b"reply\n"
        if binary_reply or home.joinpath("mode").read_text() == "tool-reply":
            for _ in range(2 if delegated else 1):
                if delegated:
                    emit({"method": "thread/realtime/transcript/delta", "params": {
                        "threadId": "fixture-thread", "role": "assistant", "delta": "fixture reply"}})
                emit({"method": "thread/realtime/transcript/done", "params": {
                    "threadId": "fixture-thread", "role": "assistant", "text": "fixture reply"}})
            if delegated:
                assert signaling.readline() == b"settle\n"
                emit({"method": "turn/completed", "params": {"threadId": "fixture-thread", "turn": {"id": "fixture-turn"}}})
                emit({"method": "thread/realtime/transcript/delta", "params": {
                    "threadId": "fixture-thread", "role": "assistant", "delta": "owed answer"}})
                emit({"method": "thread/realtime/transcript/done", "params": {
                    "threadId": "fixture-thread", "role": "assistant", "text": "owed answer"}})
            continue
        emit({"method": "turn/started", "params": {"threadId": "fixture-thread", "turn": {"id": "fixture-turn"}}})
        approval = home.joinpath("mode").read_text() == "tool-approval"
        emit({"id": "fixture-tool", "method": "item/tool/call", "params": {
            "threadId": "fixture-thread", "turnId": "fixture-turn", "callId": "fixture-call",
            "tool": "test_requires_approval" if approval else "test_echo", "arguments": {} if approval else {"text": "delegated echo"},
            "namespace": None}})
    elif method == "thread/realtime/stop":
        if home.joinpath("mode").read_text() == "tool-binary-reply":
            signaling.write(b"closing\n")
            assert signaling.readline() == b"close\n"
        emit({"method": "thread/realtime/closed", "params": {"threadId": "fixture-thread"}})
        emit({"id": request["id"], "result": {}})
    else:
        raise AssertionError("unexpected account request")
