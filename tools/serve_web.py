#!/usr/bin/env python3
"""Local browser spike server. Production uses web/server with authentication."""
import argparse
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from functools import partial
class Handler(SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()
p=argparse.ArgumentParser();p.add_argument("--dir",default=".local/web");p.add_argument("--port",type=int,default=8080);a=p.parse_args()
ThreadingHTTPServer(("127.0.0.1",a.port),partial(Handler,directory=a.dir)).serve_forever()
