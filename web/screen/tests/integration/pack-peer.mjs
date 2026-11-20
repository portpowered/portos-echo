import { execFileSync } from "node:child_process";
import { createServer } from "node:https";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { once } from "node:events";

// External archive boundary only. Production downloads use exclusive configured
// trust and real TLS; browsers request media from the production screen server.
export async function archivePeer(root, archive) {
    const openssl = (...args) =>
        execFileSync("openssl", args, { cwd: root, stdio: "pipe" });
    openssl(
        "req",
        "-x509",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
        "-days",
        "1",
        "-subj",
        "/CN=synthetic-pack-root",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-keyout",
        "ca-key.pem",
        "-out",
        "ca.pem",
    );
    openssl(
        "req",
        "-new",
        "-newkey",
        "ec",
        "-pkeyopt",
        "ec_paramgen_curve:P-256",
        "-nodes",
        "-subj",
        "/CN=synthetic-pack-peer",
        "-keyout",
        "key.pem",
        "-out",
        "server.csr",
    );
    await writeFile(
        join(root, "leaf.cnf"),
        "basicConstraints=critical,CA:FALSE\nsubjectAltName=IP:127.0.0.1\nextendedKeyUsage=serverAuth\nkeyUsage=digitalSignature\n",
    );
    openssl(
        "x509",
        "-req",
        "-in",
        "server.csr",
        "-CA",
        "ca.pem",
        "-CAkey",
        "ca-key.pem",
        "-CAcreateserial",
        "-days",
        "1",
        "-extfile",
        "leaf.cnf",
        "-out",
        "server.pem",
    );
    const requests = [];
    const peer = createServer(
        {
            key: await readFile(join(root, "key.pem")),
            cert: await readFile(join(root, "server.pem")),
        },
        (request, response) => {
            requests.push(request.url);
            response.writeHead(200, { "Content-Length": archive.length });
            response.end(archive);
        },
    );
    peer.listen(0, "127.0.0.1");
    await once(peer, "listening");
    return {
        url: `https://127.0.0.1:${peer.address().port}/template.screenpack.tar.gz`,
        requests,
        close: () =>
            new Promise((resolve) => {
                peer.closeAllConnections();
                peer.close(resolve);
            }),
    };
}
