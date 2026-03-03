#!/usr/bin/env python3
"""Simple HTTP file server for FluxDown download+delete integration tests.

Serves N small binary files without touching the filesystem.
Each file is a fixed block of bytes (repeating the file index mod 256).

Usage:
    python3 scripts/test_file_server.py [options]

Options:
    --count  N     Number of files to expose (default: 5000)
    --size   N     Bytes per file (default: 4096)
    --port   N     TCP port to listen on (default: 18080)
    --host   ADDR  Bind address (default: 127.0.0.1)

Endpoints:
    GET /file-{n}.bin     Download the n-th file (0-indexed)
    GET /list             Plain-text list of all file URLs (one per line)
    GET /status           JSON: {"count": N, "size": N}

Example:
    # Serve 5000 × 4 KB files:
    python3 scripts/test_file_server.py --count 5000 --size 4096

    # From another terminal — download URLs list:
    curl http://localhost:18080/list | head -5

    # Test a single file:
    curl -s http://localhost:18080/file-0.bin | wc -c  # should print 4096
"""

import argparse
import http.server
import json
import re
import sys
import threading
from typing import Optional


class _Handler(http.server.BaseHTTPRequestHandler):
    """Minimal HTTP/1.1 handler — zero filesystem I/O."""

    # Injected by the server factory:
    file_count: int = 0
    file_size: int = 0
    port: int = 0

    def do_GET(self) -> None:  # noqa: N802
        path = self.path.split("?", 1)[0]  # ignore query string

        # --- /file-{n}.bin ---
        m = re.fullmatch(r"/file-(\d+)\.bin", path)
        if m:
            idx = int(m.group(1))
            if 0 <= idx < self.file_count:
                data = bytes([idx % 256]) * self.file_size
                self._send(200, "application/octet-stream", data)
            else:
                self._send_error(404, f"file index {idx} out of range [0, {self.file_count})")
            return

        # --- /list ---
        if path == "/list":
            lines = [
                f"http://{self.server.server_address[0]}:{self.port}/file-{i}.bin\n"
                for i in range(self.file_count)
            ]
            body = "".join(lines).encode()
            self._send(200, "text/plain; charset=utf-8", body)
            return

        # --- /status ---
        if path == "/status":
            payload = json.dumps(
                {"count": self.file_count, "size": self.file_size}
            ).encode()
            self._send(200, "application/json", payload)
            return

        self._send_error(404, f"unknown path: {path}")

    def _send(self, code: int, content_type: str, body: bytes) -> None:
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Accept-Ranges", "bytes")
        self.end_headers()
        self.wfile.write(body)

    def _send_error(self, code: int, msg: str) -> None:
        body = msg.encode()
        self._send(code, "text/plain", body)

    def log_message(self, fmt: str, *args: object) -> None:  # noqa: N802
        # Suppress the default per-request log to keep test output clean.
        pass


def make_handler(count: int, size: int, port: int) -> type:
    """Return a handler class with injected configuration."""

    class Handler(_Handler):
        file_count = count
        file_size = size

    Handler.port = port
    return Handler


def main() -> None:
    parser = argparse.ArgumentParser(
        description="FluxDown integration-test file server"
    )
    parser.add_argument("--count", type=int, default=5000, metavar="N",
                        help="number of files to serve (default: 5000)")
    parser.add_argument("--size", type=int, default=4096, metavar="N",
                        help="bytes per file (default: 4096 = 4 KB)")
    parser.add_argument("--port", type=int, default=18080, metavar="N",
                        help="TCP port (default: 18080)")
    parser.add_argument("--host", default="127.0.0.1", metavar="ADDR",
                        help="bind address (default: 127.0.0.1)")
    args = parser.parse_args()

    handler = make_handler(args.count, args.size, args.port)

    # Allow rapid restart without TIME_WAIT delay.
    http.server.HTTPServer.allow_reuse_address = True

    server = http.server.ThreadingHTTPServer((args.host, args.port), handler)
    total_bytes = args.count * args.size

    print(
        f"FluxDown test file server\n"
        f"  Files : {args.count:,} × {args.size:,} B "
        f"= {total_bytes / 1024 / 1024:.1f} MB total\n"
        f"  Base  : http://{args.host}:{args.port}/file-{{n}}.bin  (n=0..{args.count-1})\n"
        f"  List  : http://{args.host}:{args.port}/list\n"
        f"  Status: http://{args.host}:{args.port}/status\n"
        f"Press Ctrl+C to stop.",
        flush=True,
    )

    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopped.", file=sys.stderr)
        server.shutdown()


if __name__ == "__main__":
    main()
