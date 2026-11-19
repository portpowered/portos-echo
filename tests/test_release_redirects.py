"""T3: real wget and trusted loopback TLS; recording adb, no device access."""
import http.server
import os
from pathlib import Path
import shlex
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading

from test_echo_install import require


def route_wget():
    """Only replace the fixed release origin; all redirect handling stays real."""
    args = sys.argv[2:]
    origin = "https://github.com/portpowered/portos-echo/releases/download/"
    require(args[-1].startswith("https://"), "non-HTTPS request reached wget")
    if args[-1].startswith(origin):
        args[-1] = os.environ["REDIRECT_ORIGIN"] + "/" + args[-1][len(origin):]
    os.execv(os.environ["REAL_WGET"], ["wget", "--no-proxy",
             "--ca-certificate=" + os.environ["REDIRECT_CERT"], *args])


def redirect_cases(box_class=None):
    from test_release_kit import ReleaseBox, executable

    box_class = box_class or ReleaseBox

    real_wget = shutil.which("wget")
    require(real_wget is not None, "real GNU wget required for redirect witness")
    state = {"requests": [], "http": []}

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            if self.server is plain:
                state["http"].append(self.path)
                self.send_response(200)
                self.end_headers()
                return
            state["requests"].append(self.path)
            name = self.path.rsplit("/", 1)[-1]
            targeted = name == state["asset"]
            stage = self.path.split("/")[1]
            if targeted and state["mode"] == "loop":
                location = origin + self.path
            elif targeted and state["mode"] == "direct":
                location = insecure + "/" + name
            elif targeted and state["mode"] == "later" and stage == "hop":
                location = insecure + "/" + name
            elif stage not in ("hop", "relative", "absolute", "final"):
                location = "/hop/" + name
            elif stage == "hop":
                location = "../relative/" + name
            elif stage == "relative":
                location = "//127.0.0.1:" + str(secure.server_port) + "/absolute/" + name
            elif stage == "absolute":
                location = origin + "/final/" + name
            else:
                data = (state["assets"] / name).read_bytes()
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
            self.send_response(302)
            self.send_header("Location", location)
            self.send_header("Content-Length", "0")
            self.end_headers()

    with tempfile.TemporaryDirectory(prefix="release-tls-") as temporary:
        cert, key = (Path(temporary) / name for name in ("cert.pem", "key.pem"))
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                        "-keyout", str(key), "-out", str(cert), "-days", "1",
                        "-subj", "/CN=127.0.0.1", "-addext", "subjectAltName=IP:127.0.0.1"],
                       check=True, capture_output=True, timeout=15)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        with http.server.HTTPServer(("127.0.0.1", 0), Handler) as plain, \
                http.server.HTTPServer(("127.0.0.1", 0), Handler) as secure:
            secure.socket = context.wrap_socket(secure.socket, server_side=True)
            origin = "https://127.0.0.1:" + str(secure.server_port)
            insecure = "http://127.0.0.1:" + str(plain.server_port)
            threads = [threading.Thread(target=s.serve_forever, daemon=True) for s in (plain, secure)]
            for thread in threads:
                thread.start()
            try:
                for mode, asset in (("good", "archive"), ("direct", "archive"),
                                    ("later", "archive"), ("direct", "SHA256SUMS"),
                                    ("later", "SHA256SUMS"), ("loop", "archive")):
                    with box_class(wget=True) as box:
                        state.update(mode=mode, assets=box.assets,
                                     asset=box.archive.name if asset == "archive" else asset,
                                     requests=[], http=[])
                        command = (shlex.quote(sys.executable) + " " + shlex.quote(str(Path(__file__).resolve()))
                                   + " --wget-route")
                        executable(box.tools / "wget", command)
                        box.env.update(REAL_WGET=real_wget, REDIRECT_ORIGIN=origin, REDIRECT_CERT=str(cert))
                        result = box.invoke(code=0 if mode == "good" else 1)
                        require(state["http"] == [], "HTTP downgrade was contacted")
                        if mode == "good":
                            require(len(state["requests"]) == 10, "HTTPS redirect chain differed")
                            require("Echo installed and started" in result.stdout, "HTTPS journey failed")
                            require(sum(c[0] == "push" for c in box.calls()) == 6, "payload not installed")
                        else:
                            require(box.calls() == [], "redirect failure reached adb")
                            require("download failed" in result.stderr, "redirect failure diagnostic differed")
                            if mode == "loop":
                                require(len(state["requests"]) == 11, "redirect count was not bounded")
                print("B1 real wget: HTTPS redirects=PASS archive/checksum downgrade=REFUSED HTTP requests=0 loop=BOUNDED adb=0 on failure")
            finally:
                for server in (plain, secure):
                    server.shutdown()
                for thread in threads:
                    thread.join(timeout=5)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--wget-route"]:
        route_wget()
    else:
        redirect_cases()
