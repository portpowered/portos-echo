#!/usr/bin/env python3
"""T3 fake adb/bench/bridge and HTTPS issuer, bound only to loopback.

The delivered lab owns dispatch, correlation, replay signing, counts and cleanup.
This peer owns external process/presentation observations and synthetic storage.
"""
import base64
import http.client
import http.server
import json
import os
from pathlib import Path
import shutil
import ssl
import subprocess
import sys
import threading
import time
import urllib.parse
import wave

READY = {"state": "ready", "message": "Echo Live ready"}
RELINK = {"state": "error", "relink": True,
          "message": "Run echo-bridge link with your config to relink Port OS MCP."}


def wav(path):
    with wave.open(str(path), "wb") as output:
        output.setparams((1, 2, 16000, 0, "NONE", "not compressed"))
        output.writeframes(b"\x01\x00" * 32000)


class World:
    def __init__(self, root, mode):
        self.root, self.mode = root, mode
        self.condition = threading.Condition(threading.RLock())
        self.alive, self.pid, self.generation = False, 0, 0
        self.trace, self.log, self.states = [], "", []
        self.elapsed, self.refreshes, self.plays = 0, 0, 0
        self.grant = json.loads((root / "grant.json").read_text())
        self.calls, self.replays = [], []
        self.linked = threading.Event()
        self.remote = root / "remote"
        self.remote.mkdir()
        for name, data in [("bridge.toml", "installed config sentinel\n"), ("trace.jsonl", "installed trace sentinel\n"), ("neighbor", "unowned neighbor\n")]:
            (self.remote / name).write_text(data)
        (root / "mode").write_text(mode)

    def record(self, event, **fields):
        self.elapsed += 1
        self.trace.append(dict(event=event, elapsed_ms=self.elapsed, **fields))
        with (self.remote / "lab-trace.jsonl").open("a") as output:
            output.write(json.dumps(self.trace[-1]) + "\n")

    def state(self, value):
        self.states.append(dict(seq=len(self.states) + 1, state=value))
        self.condition.notify_all()

    def boot(self):
        if (self.generation and self.mode == "launch") or self.mode == "initial-launch":
            raise ValueError("nominated launch failed")
        (self.remote / "lab-holder.pid").write_text("123\n")
        self.alive = True
        self.pid += 1
        self.generation += 1
        self.elapsed, self.trace, self.states = 0, [], []
        self.record("screen.listening", address="127.0.0.1:16900")
        self.record("mcp.operator.listening", address="127.0.0.1:16902")
        self.record("bridge.ready")
        if self.generation > 1:
            self.record("mcp.grant.reloaded")
        self.log += "client connected\n"
        self.state(READY)
        if self.mode == "cancel-start":
            (self.root / "cancel").write_text("cancel")

    def play(self, args):
        self.plays += 1
        source = Path(args[args.index("--file") + 1])
        assert source.read_bytes() == (self.root / "wake.wav").read_bytes()
        run = "query" if self.plays == 1 else "probe"
        self.record("wyoming.run_started", run_id=run)
        self.record("wyoming.mic_frames", run_id=run, count=12)
        if self.plays == 1:
            self.record("mcp.tools_list.completed", server="portos", outcome="success", count=3,
                        provider_auth_mode="chatgpt")
            self.record("tools/call", name="endpoint_query", run_id=run,
                        thread_id="thread", turn_id="turn", call_id="query")
            if self.mode != "unreturned":
                self.record("mcp.tool.completed", server="portos", name="endpoint_query",
                            attempt_count=1, outcome="error" if self.mode == "tool" else "success",
                            provider_auth_mode="chatgpt")
        else:
            assert len(self.replays) == 1
            self.state(RELINK)
        self.record("wyoming.run_ended", run_id=run)
        self.record("bridge.run_completed", run_id=run)
        if self.mode == "cancel":
            (self.root / "cancel").write_text("cancel")
        output = Path(args[args.index("--output") + 1])
        wav(output)
        return dict(held=True, playStartedAt="2026-01-01T12:00:00Z",
                    playEndedAt="2026-01-01T12:00:00.5Z",
                    recordStartedAt="2026-01-01T12:00:00Z",
                    recordEndedAt="2026-01-01T12:00:02Z")

    def adb(self, args):
        command = args[-1]
        if args[:2] == ["shell", "getprop"]:
            return {"ro.product.model": "AEOAT", "ro.product.device": "athena",
                    "ro.build.version.incremental": "0036811211140",
                    "ro.bootmode": "normal", "sys.boot_completed": "1"}[command]
        if args[:2] == ["shell", "ps"]:
            return "PID NAME\n" + (f"{self.pid} echo-bridge\n" if self.alive else "")
        if args[0] == "forward":
            if args[1] == "--remove" and self.mode == "forward":
                raise ValueError("forward removal failed")
            return str(self.port) if args[1] == "tcp:0" else ""
        if args[0] == "push":
            data = Path(args[1]).read_bytes()
            destination = Path(args[2].replace("/data/local/echo-bridge/private", str(self.remote)))
            destination.write_bytes(data[:3] if self.mode == "push" else data)
            if self.mode == "push":
                raise ValueError("partial push")
            (self.root / "pushed.toml").write_bytes(data)
            return ""
        if args[0] == "exec-out":
            if "head -c 65537" in command:
                return json.dumps(self.grant)
            if "head -c 1048577" in command:
                return (self.root / "cert.pem").read_text()
            if "head -c 16385" in command:
                return "synthetic-local-screen\n"
            if "tail -c +" in command:
                offset = int(command.split("tail -c +")[1].split()[0]) - 1
                return self.log[offset:]
            if "cat " in command:
                return "".join(json.dumps(row) + "\n" for row in self.trace)
        if "setsid" in command:
            self.boot()
        elif "kill $(cat" in command:
            if self.mode == "stop" and self.generation == 2:
                raise ValueError("stop failed")
            self.alive = False
            self.condition.notify_all()
            if self.mode == "late" and self.generation == 2:
                self.record("mcp.unlink.completed", operation_id=99)
        elif "wc -c" in command:
            return str(len(self.log))
        elif "stat -c %d:%i" in command and "sh -c" not in command:
            path = Path(command.split()[-1].replace("/data/local/echo-bridge/private", str(self.remote)))
            return str(path.stat().st_dev) + ":" + str(path.stat().st_ino)
        elif "stat -c %u:%g:%a" in command:
            path = Path(command.split()[-1].replace("/data/local/echo-bridge/private", str(self.remote)))
            return ("0:0:" if self.mode == "owner" else "9999:9999:") + ("644" if self.mode == "mode" else oct(path.stat().st_mode & 0o777)[2:])
        elif "stat -c %a" in command:
            return "700" if command.endswith("/portos") else "600"
        elif command.startswith("chown "):
            if self.mode == "chown":
                raise ValueError("chown failed")
        elif command.startswith("/data/local/hassmic/runas nobody 3990,inet "):
            fragment = command.removeprefix("/data/local/hassmic/runas nobody 3990,inet ")
            assert fragment.startswith(("mkdir ", "chmod ", "mkfifo ", "ln -T ", "sh -c ")), fragment
            fragment = fragment.replace("/data/local/echo-bridge/private", '"${ECHO_FAKE_REMOTE}"')
            if self.mode == "chmod" and fragment.startswith("chmod "):
                raise ValueError("chmod failed")
            if self.mode == "cleanup" and self.generation == 2 and "rm -f" in fragment and "lab-bridge.toml;" in fragment:
                raise ValueError("nominated cleanup failed")
            subprocess.run(["sh", "-c", fragment], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                           env=os.environ | {"ECHO_FAKE_REMOTE": str(self.remote)})
            if self.mode == "cancel-stage" and fragment.startswith("ln -T ") and fragment.endswith("lab-stderr.log"):
                (self.root / "cancel").write_text("cancel")
        else:
            raise ValueError("unsupported adb command: " + str(args))
        return ""

    def tool(self, args):
        with self.condition:
            self.calls.append(args)
            (self.root / "calls.json").write_text(json.dumps(self.calls))
            if args[0] in ("shell", "exec-out", "push", "forward", "reverse"):
                return self.adb(args)
            if args[:2] == ["session", "hold"]:
                return dict(holdId="fixture-hold", lease="fixture-lease", listening=True, talking=True)
            if args[:2] == ["session", "release"]:
                if self.mode == "release":
                    raise ValueError("release failed")
                return dict(holds=[dict(id="fixture-hold", state="ended")])
            if args[:2] == ["message", "play-audio"]:
                return self.play(args)
            raise ValueError("unsupported fake command")


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/.well-known/oauth-authorization-server":
            assert not self.headers.get("Authorization") and not self.headers.get("DPoP")
            self.send(200, dict(issuer=world.origin, token_endpoint=world.origin + "/token"))
            return
        assert self.path == "/events?token=synthetic-local-screen"
        self.close_connection = True
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        cursor, generation = 0, world.generation
        try:
            with world.condition:
                while world.alive and generation == world.generation:
                    while cursor < len(world.states):
                        frame = world.states[cursor]
                        self.wfile.write(("data: " + json.dumps(frame) + "\n\n").encode())
                        self.wfile.flush()
                        cursor += 1
                        if frame["state"]["state"] == "link_code":
                            world.linked.set()
                    world.condition.wait()
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"]))) if self.path != "/token" else None
        if self.path == "/fixture":
            try:
                self.send(200, dict(output=world.tool(body)))
            except (ValueError, AssertionError, KeyError):
                self.send(422, dict(error="fixture refusal"))
        elif self.path == "/operator/mcp":
            self.operator(body)
        elif self.path == "/token":
            self.replay()
        else:
            raise ValueError("unexpected endpoint")

    def operator(self, body):
        assert self.headers["Authorization"] == "Bearer synthetic-local-screen"
        action = body["operation"]
        if action == "link":
            with world.condition:
                world.record("mcp.link.started", operation_id=1)
                world.state(dict(state="link_code", user_code="PRIVATE-CODE-MARKER",
                                 verification_uri="https://issuer.invalid/PRIVATE-URI-MARKER",
                                 expires_at_unix=int(time.time()) + 600))
            assert world.linked.wait(5)
            with world.condition:
                world.record("mcp.link.approved", operation_id=1)
                world.state(READY)
        elif action == "refresh":
            with world.condition:
                world.refreshes += 1
                world.grant["grant"]["refresh_token"] = "synthetic-rotated-" + str(world.refreshes)
                world.record("mcp.refresh.committed", operation_id=1 if world.generation == 2 else 2)
        elif action == "unlink":
            with world.condition:
                world.record("mcp.unlink.completed", operation_id=2)
                world.grant["grant"] = None
                world.state(RELINK)
        else:
            raise ValueError("unexpected operation")
        self.send(200, dict(outcome={"link": "linked", "refresh": "refreshed", "unlink": "unlinked"}[action]))

    def replay(self):
        form = urllib.parse.parse_qs(self.rfile.read(int(self.headers["Content-Length"])).decode())
        proof = self.headers["DPoP"]
        # Preserve only synthetic input outside the product report, for independent assertions.
        world.replays.append(dict(form=form, proof=proof))
        (world.root / "replay.json").write_text(json.dumps(world.replays))
        assert form["refresh_token"] == ["synthetic-consumed"]
        assert form["grant_type"] == ["refresh_token"]
        assert form["client_id"] == [world.grant["client_id"]]
        decode = lambda part: base64.urlsafe_b64decode(part + "=" * (-len(part) % 4))
        header, claims, signature = proof.split(".")
        assert json.loads(decode(claims))["htu"] == world.origin + "/token"
        assert json.loads(decode(header))["alg"] == "ES256"
        self.send(400, dict(error="invalid_grant"))


def serve(root, mode):
    global world
    subprocess.run([
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        "-subj", "/CN=local-test", "-addext", "subjectAltName=IP:127.0.0.1",
        "-addext", "basicConstraints=critical,CA:FALSE", "-addext", "extendedKeyUsage=serverAuth",
        "-keyout", str(root / "key.pem"), "-out", str(root / "cert.pem"),
    ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    world = World(root, mode)
    plain = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    secure = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(root / "cert.pem", root / "key.pem")
    secure.socket = context.wrap_socket(secure.socket, server_side=True)
    world.port = plain.server_port
    world.origin = "https://127.0.0.1:" + str(secure.server_port)
    world.grant["issuer"] = world.origin
    threading.Thread(target=secure.serve_forever, daemon=True).start()
    (root / "port").write_text(str(world.port))
    wav(root / "wake.wav")
    print(json.dumps(dict(port=world.port, issuer=world.origin)), flush=True)
    plain.serve_forever()


if __name__ == "__main__":
    if sys.argv[1] == "--serve":
        serve(Path(sys.argv[2]), sys.argv[3])
    else:
        root = Path(os.environ["ECHO_M5_ROOT"])
        connection = http.client.HTTPConnection("127.0.0.1", int((root / "port").read_text()), timeout=5)
        connection.request("POST", "/fixture", json.dumps(sys.argv[1:]), {"Content-Type": "application/json"})
        response = connection.getresponse()
        result = json.loads(response.read())
        if response.status != 200:
            sys.exit(1)
        value = result["output"]
        print(value if isinstance(value, str) else json.dumps(value), end="")
