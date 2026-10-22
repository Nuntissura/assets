"""Loopback-only ephemeral CRM harness backed by the Rust public service."""
import argparse
import functools
import http.server
import json
import os
from pathlib import Path
import queue
import subprocess
import threading

ROOT = Path(__file__).resolve().parents[1]


def serve(port=0):
    executable = Path(os.environ.get('BB_DEMO_BINARY', ROOT / 'target/debug/examples/json_bridge'))
    if os.name == 'nt' and not executable.suffix:
        executable = executable.with_suffix('.exe')
    if not executable.is_file():
        raise RuntimeError('Build buurtman-balie-host-demo example json_bridge or configure BB_DEMO_BINARY')
    child = subprocess.Popen([str(executable)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True, encoding='utf-8')
    replies = queue.Queue(maxsize=1)
    lock = threading.Lock()
    failed = False

    def read():
        for _ in range(100_000):
            line = child.stdout.readline(32 * 1024 * 1024 + 1)
            if not line or len(line) > 32 * 1024 * 1024:
                replies.put('{"error":"CRM bridge response missing or oversized"}')
                return
            replies.put(line)
    threading.Thread(target=read, daemon=True).start()

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            nonlocal failed
            origin = f'http://127.0.0.1:{self.server.server_port}'
            if self.path != '/api' or self.headers.get('Origin') != origin or self.headers.get('Host') != origin[7:]:
                self.send_error(403)
                return
            try:
                size = int(self.headers.get('Content-Length', '0'))
                if not 0 < size <= 131_000 or self.headers.get('Content-Type') != 'application/json':
                    self.send_error(413)
                    return
                self.connection.settimeout(10)
                payload = json.loads(self.rfile.read(size))
                with lock:
                    if failed:
                        self.send_error(503, 'Restart the uncertain CRM bridge')
                        return
                    child.stdin.write(json.dumps(payload, ensure_ascii=False) + '\n')
                    child.stdin.flush()
                    response = replies.get(timeout=10).encode('utf-8')
                self.send_response(200)
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(response)))
                self.end_headers()
                self.wfile.write(response)
            except (ValueError, OSError, queue.Empty):
                failed = True
                self.send_error(503, 'CRM bridge unavailable')

    class Server(http.server.ThreadingHTTPServer):
        daemon_threads = True
        request_queue_size = 8
        slots = threading.BoundedSemaphore(8)

        def get_request(self):
            connection, address = super().get_request()
            connection.settimeout(10)
            return connection, address

        def process_request(self, request, address):
            if not self.slots.acquire(blocking=False):
                request.close()
                return
            try:
                super().process_request(request, address)
            except Exception:
                self.slots.release()
                raise

        def process_request_thread(self, request, address):
            try:
                super().process_request_thread(request, address)
            finally:
                self.slots.release()

    server = Server(('127.0.0.1', port), functools.partial(Handler, directory=str(ROOT / 'web')))
    return server, child


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--port', type=int, default=0)
    args = parser.parse_args()
    server, child = serve(args.port)
    print(f'Buurtman Balie temporary-data harness: http://127.0.0.1:{server.server_port}', flush=True)
    try:
        server.serve_forever()
    finally:
        server.server_close()
        child.stdin.close()
        child.terminate()
        child.wait(timeout=5)
