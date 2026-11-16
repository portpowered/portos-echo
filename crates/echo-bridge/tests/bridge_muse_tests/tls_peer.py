"""T3 controlled HTTP event sequence over verified loopback TLS; no remote calls."""
import http.server
import json
import ssl
import sys
import threading
from pathlib import Path


requests = []
credential = Path("muse-credential").read_text()


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def respond(self, status, body=None):
        payload = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def authorized(self):
        if self.headers.get("Authorization") != "Bearer " + credential:
            self.respond(401)
            return False
        return True

    def do_POST(self):
        if not self.authorized():
            return
        length = int(self.headers["Content-Length"])
        assert 0 < length <= 65536
        body = json.loads(self.rfile.read(length))
        requests.append({"method": "POST", "path": self.path, "body": body})
        self.respond(201, {"task_id": "task-pending"})

    def do_GET(self):
        if not self.authorized():
            return
        requests.append({"method": "GET", "path": self.path})
        index = len(requests) - 2
        if index == 0:
            self.respond(204)
        elif index == 1:
            self.respond(200, {"type": "progress", "task_id": "task-pending",
                               "message": "Still working"})
        elif index == 2:
            self.respond(200, {"type": "completed", "task_id": "task-pending",
                               "summary": "Muse test job completed"})
        else:
            self.respond(500)


server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("server.crt", "server.pem")
server.socket = context.wrap_socket(server.socket, server_side=True)
worker = threading.Thread(target=server.serve_forever)
worker.start()
print(server.server_address[1], flush=True)
try:
    # EOF also gracefully closes the peer after an assertion in the Rust owner.
    sys.stdin.readline()
finally:
    server.shutdown()
    worker.join()
    server.server_close()
print(json.dumps(requests), flush=True)
