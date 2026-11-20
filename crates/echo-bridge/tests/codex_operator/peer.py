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


trace_turn = "fixture-turn"


def tool_request(approval=False):
    emit({"id": "fixture-tool", "method": "item/tool/call", "params": {
        "threadId": "fixture-thread", "turnId": trace_turn, "callId": "fixture-call",
        "tool": "test_requires_approval" if approval else "test_echo",
        "arguments": {} if approval else {"text": "delegated echo"}, "namespace": None}})


def trace_exit(mode):
    cause = mode.removeprefix("tool-trace-exit-")
    connection, _ = control.accept()
    with connection:
        assert connection.recv(64) == b"exit"
    if cause == "child":
        os._exit(0)
    if cause in ("shutdown", "kill", "media"):
        return
    if cause == "timeout":
        tool_request(True)
        return
    if cause == "interrupt":
        emit({"id": "fixture-refusal", "method": "item/tool/call", "params": {
            "threadId": "fixture-thread", "turnId": "fixture-turn", "callId": "fixture-refusal",
            "tool": "unknown_fixture_tool", "arguments": {}, "namespace": None}})
        return
    if cause in ("error", "closed"):
        emit({"method": "thread/realtime/" + ("error" if cause == "error" else "closed"),
              "params": {"threadId": "fixture-thread", "message": "synthetic-private-error"}})
        return
    status = "failed" if cause == "failed" else "completed"
    for _ in range(2):
        emit({"method": "turn/completed", "params": {"threadId": "fixture-thread", "turn": {
            "id": "" if cause == "malformed" else "fixture-turn", "status": status}}})
    emit({"method": "item/completed", "params": {"threadId": "fixture-thread", "turnId": "fixture-turn",
          "item": {"type": "agentMessage", "id": "late-answer", "phase": "final_answer", "text": "late-not-an-answer"}}})


for line in sys.stdin:
    request = json.loads(line)
    if "method" not in request:
        if home.joinpath("mode").read_text().startswith("tool-trace-exit-"):
            assert request["id"] in ("fixture-tool", "fixture-refusal")
            if request["id"] == "fixture-refusal":
                tool_request()
            continue
        assert request["id"] == "fixture-tool"
        if home.joinpath("mode").read_text().startswith("tool-trace"):
            # The RPC result and item completion name the same logical call.
            emit({"method": "item/completed", "params": {
                "threadId": "fixture-thread", "turnId": trace_turn, "item": {
                    "type": "dynamicToolCall", "id": "fixture-call",
                    "tool": "test_requires_approval" if home.joinpath("mode").read_text() == "tool-trace-approval" else "test_echo",
                    "arguments": {} if home.joinpath("mode").read_text() == "tool-trace-approval" else {"text": "delegated echo"},
                    "status": "completed", "contentItems": request["result"]["contentItems"],
                    "success": request["result"]["success"]}}})
            emit({"method": "item/completed", "params": {
                "threadId": "fixture-thread", "turnId": trace_turn, "item": {
                    "type": "agentMessage", "id": "answer-7", "phase": "final_answer",
                    "text": "synthetic-result-7"}}})
            for _ in range(2):
                emit({"method": "turn/completed", "params": {
                    "threadId": "fixture-thread", "turn": {"id": trace_turn, "status": "completed"}}})
        home.joinpath("tool-result").write_text(json.dumps(request["result"]))
        if home.joinpath("mode").read_text() == "tool-trace-isolation" and trace_turn == "fixture-turn":
            trace_turn = "fixture-turn-2"
            emit({"method": "thread/realtime/itemAdded", "params": {"threadId": "fixture-thread", "item": {
                "type": "handoff_request", "handoff_id": "delegation-8", "item_id": "handoff-8",
                "input_transcript": "second task", "active_transcript": []}}})
            emit({"method": "turn/started", "params": {"threadId": "fixture-thread", "turn": {"id": trace_turn}}})
            for thread, turn in (("foreign-thread", trace_turn), ("fixture-thread", "unknown-turn"), ("fixture-thread", "fixture-turn")):
                emit({"method": "item/completed", "params": {"threadId": thread, "turnId": turn, "item": {
                    "type": "mcpToolCall", "id": "foreign-item", "tool": "foreign_tool", "server": "foreign-server",
                    "status": "completed", "result": {"content": []}}}})
            emit({"method": "turn/completed", "params": {"threadId": "fixture-thread", "turn": {
                "id": "fixture-turn", "status": "completed"}}})
            tool_request()
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
    elif method == "account/logout":
        assert request.get("params") is None
        assert home.joinpath("mode").read_text() == "screen-account"
        home.joinpath("auth.json").unlink()
        emit({"id": request["id"], "result": {}})
    elif method == "account/read":
        assert request["params"] == {"refreshToken": False}
        mode = home.joinpath("mode").read_text()
        if mode == "startup-network":
            try:
                with socket.create_connection(("127.0.0.1", int(home.joinpath("provider-port").read_text()))):
                    outcome = "accepted"
            except ConnectionRefusedError:
                outcome = "refused"
            with home.joinpath("provider-attempts").open("a") as attempts:
                attempts.write(outcome + "\n")
            if outcome == "refused":
                emit({"id": request["id"], "error": {
                    "code": -32603, "message": "workspace routing discovery failed",
                    "data": "synthetic-private-detail"}})
                continue
        if mode in ("startup-timeout", "startup-unauthorized"):
            message = "workspace routing discovery " + (
                "timed out" if mode == "startup-timeout" else "unauthorized (401)")
            emit({"id": request["id"], "error": {
                "code": -32603, "message": message}})
            continue
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
        if mode == "signed-out" or (mode == "screen-account" and not home.joinpath("auth.json").is_file()):
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
            continue
        if home.joinpath("mode").read_text().startswith("tool-trace"):
            emit({"method": "thread/realtime/itemAdded", "params": {
                "threadId": "fixture-thread", "item": {"type": "handoff_request",
                    "handoff_id": "delegation-7", "item_id": "handoff-7",
                    "input_transcript": "synthetic-query-7", "active_transcript": []}}})
        early = home.joinpath("mode").read_text() == "tool-trace-early"
        if early:
            tool_request()
        emit({"method": "turn/started", "params": {"threadId": "fixture-thread", "turn": {"id": "fixture-turn"}}})
        if home.joinpath("mode").read_text().startswith("tool-trace-exit-"):
            trace_exit(home.joinpath("mode").read_text())
            continue
        if home.joinpath("mode").read_text().startswith("tool-trace"):
            search = {"type": "webSearch", "id": "search-7", "query": "synthetic-query-7",
                      "action": {"type": "search", "query": "synthetic-query-7", "queries": None}}
            emit({"method": "item/started", "params": {
                "threadId": "fixture-thread", "turnId": "fixture-turn", "item": search}})
            search["results"] = [{"url": "http://127.0.0.1/synthetic-source", "text": "synthetic-result-7",
                                  "account_id": "synthetic-account", "authorization": "synthetic-auth"}]
            emit({"method": "item/completed", "params": {
                "threadId": "fixture-thread", "turnId": "fixture-turn", "item": search}})
        if not early:
            tool_request(home.joinpath("mode").read_text() in ("tool-approval", "tool-trace-approval"))
    elif method == "turn/interrupt":
        assert home.joinpath("mode").read_text() in ("tool-trace-exit-timeout", "tool-trace-exit-interrupt")
        assert request["params"] == {"threadId": "fixture-thread", "turnId": "fixture-turn"}
        emit({"id": request["id"], "result": {}})
        for status in ("interrupted", "completed", "interrupted"):
            emit({"method": "turn/completed", "params": {"threadId": "fixture-thread",
                  "turn": {"id": "fixture-turn", "status": status}}})
    elif method == "thread/realtime/stop":
        if home.joinpath("mode").read_text() == "tool-binary-reply":
            signaling.write(b"closing\n")
            assert signaling.readline() == b"close\n"
        emit({"method": "thread/realtime/closed", "params": {"threadId": "fixture-thread"}})
        emit({"id": request["id"], "result": {}})
    else:
        raise AssertionError("unexpected account request")
