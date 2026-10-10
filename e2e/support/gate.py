"""A mirror proxy run inside a test container that holds one request until the test lets it go.

Usage: gate.py LISTEN_HOST PORT UPSTREAM_HOST PATTERN DIR

Once it listens, the proxy writes a line to the FIFO DIR/ready. The first request whose path
matches PATTERN is held: the proxy writes the path to the FIFO
DIR/reached (which blocks until the test reads it), then waits for a line on the FIFO
DIR/release before forwarding the request. Every other request is forwarded at once. Every
path it sees is appended to DIR/log.
"""

import http.client
import http.server
import re
import sys
import threading

LISTEN_HOST, PORT, UPSTREAM_HOST, PATTERN, DIRECTORY = sys.argv[1:6]
PATTERN_RE = re.compile(PATTERN)
HELD = threading.Lock()
HOLDING = threading.Event()


def hold(path: str) -> None:
    with HELD:
        with open(f"{DIRECTORY}/log", "a") as log:
            log.write(path + "\n")
        if HOLDING.is_set() or not PATTERN_RE.search(path):
            return
        HOLDING.set()
    with open(f"{DIRECTORY}/reached", "w") as reached:
        reached.write(path + "\n")
    with open(f"{DIRECTORY}/release") as release:
        release.readline()


class Proxy(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def forward(self, method: str) -> None:
        hold(self.path)
        upstream = http.client.HTTPConnection(UPSTREAM_HOST, int(PORT))
        headers = {key: value for key, value in self.headers.items() if key.lower() != "host"}
        headers["Host"] = self.headers.get("Host", f"{UPSTREAM_HOST}:{PORT}")
        upstream.request(method, self.path, headers=headers)
        response = upstream.getresponse()
        body = response.read()
        self.send_response(response.status, response.reason)
        for key, value in response.getheaders():
            if key.lower() not in ("transfer-encoding", "connection", "content-length"):
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if method != "HEAD":
            self.wfile.write(body)
        upstream.close()

    def do_GET(self):
        self.forward("GET")

    def do_HEAD(self):
        self.forward("HEAD")


server = http.server.ThreadingHTTPServer((LISTEN_HOST, int(PORT)), Proxy)
server.daemon_threads = True
with open(f"{DIRECTORY}/ready", "w") as ready:
    ready.write("listening\n")
server.serve_forever()
