#!/usr/bin/env python3
"""Open the app's window in a browser, against a running daemon.

    scripts/dev-ui.py [port]          # then open http://127.0.0.1:<port>

The window is plain HTML, CSS and JavaScript; the only thing the app adds is
one command that forwards to the daemon. This serves the same files with that
command done over HTTP, so the interface can be worked on, and driven by
browser tests, without building the app. It reads the daemon's address from
its data directory (LLMPOOL_HOME, as everywhere), so pointing it at a
throwaway directory keeps real logins out of it. Loopback only.
"""
import json
import os
import pathlib
import sys
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UI = pathlib.Path(__file__).resolve().parent.parent / "crates/llmpool-gui/ui"
SHIM = """window.__TAURI__ = {
  core: { invoke: async (command, args) => {
    const r = await fetch("/__invoke/" + command, { method: "POST", body: JSON.stringify(args ?? {}) });
    const text = await r.text();
    if (!r.ok) throw text;
    return text ? JSON.parse(text) : null;
  } },
  event: { listen: async () => () => {} },
};"""
TYPES = {".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png"}


def data_dir():
    if os.environ.get("LLMPOOL_HOME"):
        return pathlib.Path(os.environ["LLMPOOL_HOME"])
    return pathlib.Path.home() / "Library/Application Support/llmpool"


def daemon(method, path, body):
    where = json.loads((data_dir() / "daemon.json").read_text())
    request = urllib.request.Request(
        f"http://127.0.0.1:{where['port']}{path}",
        method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {where['token']}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return 200, response.read()
    except urllib.error.HTTPError as e:
        return 400, json.dumps(json.loads(e.read() or b"{}").get("error", "request failed")).encode()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, status, body, kind="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", kind)
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        name = self.path.split("?")[0].lstrip("/") or "index.html"
        if name == "__tauri.js":
            return self.reply(200, SHIM.encode(), "text/javascript")
        path = (UI / name).resolve()
        if UI not in path.parents or not path.is_file():
            return self.reply(404, b"not found", "text/plain")
        body = path.read_bytes()
        if name == "index.html":
            body = body.replace(b'<script src="icons.js">', b'<script src="/__tauri.js"></script><script src="icons.js">')
        self.reply(200, body, TYPES.get(path.suffix, "application/octet-stream"))

    def do_POST(self):
        args = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        command = self.path.rsplit("/", 1)[-1]
        if command == "api":
            try:
                return self.reply(*daemon(args["method"], args["path"], args.get("body")))
            except OSError as e:
                return self.reply(400, json.dumps(f"daemon unreachable: {e}").encode())
        # What only the app can do (start at login, run setup) has no meaning here.
        self.reply(200, json.dumps("not available outside the app").encode())


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 4510
    print(f"http://127.0.0.1:{port}  (daemon in {data_dir()})")
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
