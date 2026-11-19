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

    def do_GET(self):
        report["requests"] += 1
        report["discovery_credentials"] = bool(
            self.headers.get("Authorization") or self.headers.get("DPoP")
            or self.headers.get("Content-Length") not in (None, "0")
        )
        assert self.path == "/.well-known/oauth-authorization-server"
        if mode == "hang":
            print("pending", flush=True)
            report["closed"] = self.rfile.read(1) == b""
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


server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
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
