"""T3 loopback HTTPS peer; ephemeral synthetic key/certificate outside git."""
import http.server
import json
from pathlib import Path
import ssl
import subprocess
import sys
import urllib.parse

root, mode = Path(sys.argv[1]), sys.argv[2]
subprocess.run([
    "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
    "-subj", "/CN=local-test", "-addext", "subjectAltName=IP:127.0.0.1",
    "-addext", "basicConstraints=critical,CA:FALSE",
    "-addext", "extendedKeyUsage=serverAuth",
    "-keyout", str(root / "key.pem"), "-out", str(root / "cert.pem"),
], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
report = {"requests": 0, "discovery_credentials": False}


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, status, body):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def cancelled(self):
        print("pending", flush=True)
        try:
            try:
                closed = self.rfile.read(1) == b""
            except (BrokenPipeError, ConnectionResetError, ssl.SSLEOFError):
                closed = True
            if closed and mode != "hang":
                self.write_after_close()
            report["closed"] = closed
        except (BrokenPipeError, ConnectionResetError, ssl.SSLEOFError):
            # Only cancellation I/O normalizes client-initiated closure.
            report["closed"] = True

    def write_after_close(self):
        # Choose the writer's exception deterministically after real TLS closure,
        # before recording success. Do not rely on the kernel choosing an errno.
        error = {
            "hang-write-after-close": BrokenPipeError,
            "hang-reset-after-close": ConnectionResetError,
            "hang-unexpected-after-close": ValueError,
        }[mode]
        report["write_attempted"] = True
        report["write_error"] = error.__name__
        raise error()

    def do_GET(self):
        report["requests"] += 1
        report["discovery_credentials"] = bool(
            self.headers.get("Authorization") or self.headers.get("DPoP")
            or self.headers.get("Content-Length") not in (None, "0")
        )
        assert self.path == "/.well-known/oauth-authorization-server"
        if mode == "hang" or mode.endswith("-after-close"):
            self.cancelled()
            return
        self.send(302 if mode == "redirect" else 200, {
            "issuer": origin + "/wrong" if mode == "wrong-issuer" else origin,
            "token_endpoint": "https://127.0.0.1:1/token" if mode == "foreign-token" else origin + "/token",
            "extra": "x" * 65537 if mode == "oversize" else "",
        })

    def do_POST(self):
        report["requests"] += 1
        assert self.path == "/token"
        report["proof"] = self.headers["DPoP"]
        report["form"] = urllib.parse.parse_qs(self.rfile.read(int(self.headers["Content-Length"])).decode())
        self.send(200 if mode == "success" else 400,
                  {"error": "invalid_dpop_proof" if mode == "wrong-error" else "invalid_grant"})


class Server(http.server.HTTPServer):
    def handle_error(self, *_):
        # Preserve the failure without dumping the synthetic request or proof.
        # HTTPServer otherwise swallows handler errors into the silenced stderr.
        error = sys.exc_info()[1]
        report["peer_error"] = {
            "type": type(error).__name__,
            "errno": getattr(error, "errno", None),
            "reason": getattr(error, "reason", None),
        }


server = Server(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(root / "cert.pem", root / "key.pem")
server.socket = context.wrap_socket(server.socket, server_side=True)
origin = "https://127.0.0.1:" + str(server.server_port)
print(origin, flush=True)
server.handle_request()
if mode in ("reject", "success", "wrong-error"):
    server.handle_request()
server.server_close()
print(json.dumps(report), flush=True)
