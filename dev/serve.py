#!/usr/bin/env python3
"""Serves the repo on 127.0.0.1 for the UI preview harness, with the app's CSP on ui/ pages.

usage: python3 dev/serve.py [port]   (default 5174), then open http://127.0.0.1:5174/dev/?s=list
"""
import functools
import http.server
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CSP = json.load(open(os.path.join(ROOT, "src-tauri", "tauri.conf.json")))["app"]["security"]["csp"]


class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        if self.path.startswith("/ui/"):
            self.send_header("Content-Security-Policy", CSP)
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *args):
        pass


port = int(sys.argv[1]) if len(sys.argv) > 1 else 5174
server = http.server.ThreadingHTTPServer(("127.0.0.1", port), functools.partial(Handler, directory=ROOT))
print(f"http://127.0.0.1:{port}/dev/?s=list")
server.serve_forever()
