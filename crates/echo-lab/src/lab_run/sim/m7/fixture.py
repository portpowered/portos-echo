"""T3 only: ephemeral trust and private callback using prebuilt local binaries."""
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys

root, binaries = map(Path, sys.argv[1:])
os.umask(0o077)


def openssl(*args):
    subprocess.run(["openssl", *args], cwd=root, check=True, capture_output=True,
                   timeout=10)


openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        "-subj", "/CN=synthetic", "-addext", "subjectAltName=IP:127.0.0.1",
        "-addext", "basicConstraints=critical,CA:FALSE",
        "-addext", "extendedKeyUsage=serverAuth",
        "-keyout", "server.pem", "-out", "server.crt")
openssl("x509", "-in", "server.crt", "-outform", "DER", "-out", "server.der")
openssl("pkcs8", "-topk8", "-nocrypt", "-in", "server.pem", "-outform", "DER",
        "-out", "server.key")
(root / "muse-credential").write_text(os.urandom(32).hex())
shutil.copyfile(binaries / "echo-muse-fake-gadget", root / "gadget")
(root / "gadget").chmod(0o700)
(root / "script.json").write_text(json.dumps({
    "stdout": "Done! The job completed successfully.", "stderr": "", "exit": 0}))
(root / "daemon.json").write_text(json.dumps({
    "listen": "127.0.0.1:0", "tls_certificate_der": str(root / "server.der"),
    "tls_private_key_der": str(root / "server.key"),
    "client_credential_file": str(root / "muse-credential"),
    "store": str(root / "tasks.json"), "report_socket": str(root / "report.sock"),
    "gadget_program": str(root / "gadget"),
    "clock": {"source": "controlled_stdin", "initial_unix_seconds": 100}}))
print(json.dumps({"event": "m7.prepared"}), flush=True)
for line in sys.stdin:
    request = json.loads(line)
    assert set(request) == {"task_id"}
    environment = dict(os.environ, ECHO_MUSE_REPORT_SOCKET=str(root / "report.sock"))
    result = subprocess.run([str(binaries / "echo-muse-report")], env=environment,
                            input=json.dumps({"task_id": request["task_id"],
                                              "status": "completed",
                                              "summary": "Muse test job completed"}),
                            text=True, capture_output=True, timeout=5)
    print(json.dumps({"event": "m7.report", "exit": result.returncode,
                      "stderr": result.stderr, "stdout": result.stdout}), flush=True)
