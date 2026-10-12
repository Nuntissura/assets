"""Loopback-only static host for the pilot and an explicitly selected generated package."""
from __future__ import annotations

import argparse
import json
import mimetypes
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit


def main() -> None:
    project = Path(__file__).resolve().parents[3]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pkg", type=Path, default=project / ".local/web/pkg")
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    web = Path(__file__).resolve().parents[1] / "web"
    package = args.pkg.resolve(strict=True)
    for name in ("kumihimo_web.js", "kumihimo_web_bg.wasm"):
        if not (package / name).is_file():
            parser.error(f"generated package missing {name}")
    if not 0 <= args.port <= 65535:
        parser.error("port must be between 0 and 65535")
    mimetypes.add_type("application/javascript", ".mjs")
    mimetypes.add_type("application/javascript", ".js")
    mimetypes.add_type("application/wasm", ".wasm")

    class Handler(SimpleHTTPRequestHandler):
        def translate_path(self, path: str) -> str:
            decoded = unquote(urlsplit(path).path)
            parts = decoded.split("/")
            root = package if parts[1:2] == ["pkg"] else web
            relative = parts[2:] if root == package else parts[1:]
            if any(part in ("..", ".") or "\\" in part or ":" in part for part in relative):
                return str(root / "__invalid_path__")
            candidate = root.joinpath(*relative).resolve()
            if not candidate.is_relative_to(root):
                return str(root / "__invalid_path__")
            return str(candidate)

        def end_headers(self) -> None:
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            super().end_headers()

        def list_directory(self, path: str):
            self.send_error(404, "Directory listing disabled")
            return None

        def log_message(self, format: str, *arguments) -> None:
            pass

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print(json.dumps({"ready": True, "url": f"http://127.0.0.1:{server.server_port}/", "web": str(web), "pkg": str(package)}), flush=True)
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
