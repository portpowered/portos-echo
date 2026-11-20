"""T3: real curl/wget and trusted loopback TLS; recording installer, no device access."""
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


def route_download():
    """Only replace the fixed release origin; all redirect handling stays real."""
    client, *args = sys.argv[2:]
    origin = "https://github.com/"
    require(args[-1].startswith("https://"), "non-HTTPS request reached downloader")
    if args[-1].startswith(origin):
        args[-1] = os.environ["REDIRECT_ORIGIN"] + "/" + args[-1][len(origin):]
    require(args[-1].startswith(os.environ["REDIRECT_ORIGIN"] + "/"), "non-loopback downloader target")
    if client == "wget":
        args = ["--no-proxy", "--ca-certificate=" + os.environ["REDIRECT_CERT"], *args]
    else:
        args[1:1] = ["--noproxy", "*", "--cacert", os.environ["REDIRECT_CERT"]]
    os.execv(os.environ["REAL_DOWNLOADER"], [client, *args])


def redirect_cases():
    from test_release_kit import ReleaseBox, executable

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
                for client in ("curl", "wget"):
                    real = shutil.which(client)
                    require(real is not None, "real " + client + " required for redirect witness")
                    for mode, asset in (("good", "installer"), ("direct", "installer"),
                                        ("later", "installer"), ("direct", "SHA256SUMS"),
                                        ("later", "SHA256SUMS"), ("loop", "installer"), ("loop", "SHA256SUMS")):
                        with ReleaseBox(wget=client == "wget") as box:
                            state.update(mode=mode, assets=box.assets,
                                         asset=box.installer.name if asset == "installer" else asset,
                                         requests=[], http=[])
                            command = (shlex.quote(sys.executable) + " " + shlex.quote(str(Path(__file__).resolve()))
                                       + " --route " + client)
                            executable(box.tools / client, command)
                            box.env.update(REAL_DOWNLOADER=real, REDIRECT_ORIGIN=origin, REDIRECT_CERT=str(cert))
                            result = box.invoke(code=0 if mode == "good" else 1)
                            require(state["http"] == [] and box.calls() == [], "unexpected HTTP or adb call")
                            if mode == "good":
                                require(len(state["requests"]) == 10, "HTTPS redirect chain differed")
                                require(box.installer_args() == ["install", "--version", box.version], "HTTPS journey failed")
                            else:
                                require(box.installer_args() is None, "redirect failure executed the installer")
                                require(result.stderr == "echo-release: download failed\n", "redirect failure diagnostic differed")
                                if mode == "loop":
                                    expected = 11 if asset == "installer" else 16
                                    require(len(state["requests"]) == expected, "redirect count was not bounded")
                    print(f"F31 real {client}: HTTPS redirects=PASS installer/checksum downgrade=REFUSED HTTP requests=0 loop=BOUNDED")
            finally:
                for server in (plain, secure):
                    server.shutdown()
                for thread in threads:
                    thread.join(timeout=5)


if __name__ == "__main__":
    if sys.argv[1:2] == ["--route"]:
        route_download()
    else:
        redirect_cases()
