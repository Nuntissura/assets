#!/usr/bin/env python3
"""Owned Linux Servo 0.6 screenshot bridge; JSON lines, independent admission.

No executable is invoked at initialization. PRELAUNCH is a stop point: the
external root controller must inspect the worker's kernel identity, then return
a fresh independent-root receipt. This code is not an isolation proof producer.
Pinned routes/arguments: Servo v0.6.0 ports/servoshell/prefs.rs and
components/webdriver_server/lib.rs. All paths come from a local --config file.
"""
import argparse
import base64
import hashlib
import http.client
import json
import os
from pathlib import Path
import select
import signal
import struct
import subprocess
import sys
import time
import uuid
import zlib

PROTOCOL = "kumihimo.servo-worker.v1"
SERVO_COMMIT = "c78d2c206f80a1c8b67eefa97f773bba513205d3"
BINARY_SHA256 = "cb9267dfdbd89d0678cb4859857708a2c3c50affff2c091770ff73341263b09d"
SCOPE_KEYS = ("identity", "session", "provider", "resource", "target", "connection", "instance", "mount")
DOM_SCRIPT = """return {width:innerWidth,height:innerHeight,dpr:devicePixelRatio,
 title:document.title,url:location.href,state:window.kumihimoState || null,
 text:document.body.innerText,input:document.querySelector('input').value,
 button:(function(){var r=document.querySelector('button').getBoundingClientRect();
 return {x:r.x,y:r.y,width:r.width,height:r.height};})(),
 field:(function(){var r=document.querySelector('input').getBoundingClientRect();
 return {x:r.x,y:r.y,width:r.width,height:r.height};})()};"""


class BridgeError(Exception):
    pass


def now_ms():
    return time.time_ns() // 1_000_000


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_digest(path):
    h = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def proc_record(pid):
    root = Path("/proc") / str(pid)
    try:
        text = (root / "stat").read_text()
        fields = text[text.rfind(")") + 2:].split()
        return {"pid": pid, "starttime_ticks": int(fields[19]),
                "network_namespace": os.readlink(root / "ns/net"),
                "ppid": int(fields[1]), "state": fields[0],
                "cpu_ticks": int(fields[11]) + int(fields[12]),
                "rss_bytes": int(fields[21]) * os.sysconf("SC_PAGE_SIZE"),
                "observed_unix_ms": now_ms(), "clock_ticks_per_second": os.sysconf("SC_CLK_TCK")}
    except (FileNotFoundError, ProcessLookupError):
        return None


def identity(record):
    if not record:
        raise BridgeError("owned process absent")
    return {key: record[key] for key in ("pid", "starttime_ticks", "network_namespace")}


def positive(value, label, maximum=None):
    if type(value) is not int or value < 1 or (maximum is not None and value > maximum):
        raise BridgeError("invalid " + label)
    return value


def png_dimensions(data, max_side, max_raw):
    if len(data) < 33 or data[:8] != b"\x89PNG\r\n\x1a\n":
        raise BridgeError("engine did not return PNG")
    offset, dims, ended = 8, None, False
    while offset < len(data):
        if offset + 12 > len(data):
            raise BridgeError("truncated PNG chunk")
        length = struct.unpack(">I", data[offset:offset + 4])[0]
        end = offset + 12 + length
        if end > len(data):
            raise BridgeError("PNG chunk exceeds payload")
        chunk_type = data[offset + 4:offset + 8]
        payload = data[offset + 8:offset + 8 + length]
        crc = struct.unpack(">I", data[offset + 8 + length:end])[0]
        if zlib.crc32(chunk_type + payload) & 0xffffffff != crc:
            raise BridgeError("PNG CRC mismatch")
        if offset == 8:
            if chunk_type != b"IHDR" or length != 13:
                raise BridgeError("PNG missing first IHDR")
            width, height = struct.unpack(">II", payload[:8])
            positive(width, "PNG width", max_side)
            positive(height, "PNG height", max_side)
            if width * height * 4 > max_raw:
                raise BridgeError("PNG decoded RGBA bound exceeded")
            if payload[8] != 8 or payload[9] not in (2, 6):
                raise BridgeError("unsupported engine PNG sample format")
            dims = (width, height)
        if chunk_type == b"IEND":
            if length != 0 or end != len(data):
                raise BridgeError("invalid PNG ending")
            ended = True
        offset = end
    if not ended:
        raise BridgeError("PNG missing IEND")
    return dims


class Worker:
    def __init__(self, config):
        self.config = config
        self.max_message = positive(config.get("max_message_bytes", 4 * 1024 * 1024), "message cap", 4 * 1024 * 1024)
        self.max_png = positive(config.get("max_png_bytes", 16 * 1024 * 1024), "PNG cap", 16 * 1024 * 1024)
        self.max_raw = positive(config.get("max_rgba_bytes", 16 * 1024 * 1024), "RGBA cap", 16 * 1024 * 1024)
        self.max_side = positive(config.get("max_side", 2048), "side cap", 2048)
        self.max_engines = positive(config.get("max_engines", 2), "engine cap", 2)
        self.max_frames = positive(config.get("max_host_frames", 4), "host frame cap")
        self.max_events = positive(config.get("max_events", 256), "event history cap", 1024)
        self.max_request_history = positive(config.get("max_request_history", 1024), "request history cap", 4096)
        if self.max_events < 16:
            raise BridgeError("event budget must reserve lifecycle/stop diagnostics")
        self.output = Path(config["output_root"]).resolve()
        self.runtime = Path(config["runtime_executable"]).resolve()
        self.fixtures = Path(config["fixture_root"]).resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.worker = identity(proc_record(os.getpid()))
        self.engines = {}
        self.challenge = None
        self.used_receipts = set()
        self.seen_requests = set()
        self.events = []
        self.captured_frames = []
        self.input_buffer = bytearray()
        self.response_request = None
        self.unload_request_id = None
        self.scenario_end = time.monotonic() + min(240, positive(config.get("scenario_seconds", 240), "scenario seconds", 240))
        self.prepare_launch()

    def emit(self, event, **values):
        if len(self.events) >= self.max_events:
            raise BridgeError("bounded event history exhausted")
        if self.response_request is not None and "request" not in values and event not in ("PRELAUNCH", "CAPTURE_READY", "STOP", "SCENARIO_OBSERVED", "SCENARIO_FAILED"):
            values["request"] = self.response_request
        result = {"protocol": PROTOCOL, "event": event, "worker": self.worker,
                  "observed_unix_ms": now_ms(), **values}
        raw = json.dumps(result, separators=(",", ":"), allow_nan=False).encode()
        if len(raw) > self.max_message:
            raise BridgeError("outbound message exceeds cap")
        sys.stdout.buffer.write(raw + b"\n")
        sys.stdout.buffer.flush()
        self.events.append(result)
        return result

    def check_time(self):
        if time.monotonic() >= self.scenario_end:
            raise BridgeError("product scenario deadline exceeded")
        if identity(proc_record(os.getpid())) != self.worker:
            raise BridgeError("worker kernel identity changed")

    def prepare_launch(self, view_key=None):
        self.check_time()
        self.challenge = {"request_id": uuid.uuid4().hex, "issued_unix_ms": now_ms(), "view_key": view_key}
        return self.emit("PRELAUNCH", **self.challenge, stop=True,
                         required_receipt="independent-root kernel admission before ANY executable or WebDriver")

    def admission(self, receipt, challenge):
        self.check_time()
        if not isinstance(receipt, dict) or receipt.get("issuer") != "independent-root":
            raise BridgeError("missing independent-root admission")
        if receipt.get("worker") != self.worker or receipt.get("request_id") != challenge["request_id"]:
            raise BridgeError("admission worker/request mismatch")
        receipt_id = receipt.get("receipt_id")
        if not isinstance(receipt_id, str) or not receipt_id or receipt_id in self.used_receipts:
            raise BridgeError("missing/reused admission receipt id")
        if len(self.used_receipts) >= self.max_request_history:
            raise BridgeError("bounded receipt history exhausted")
        observed, expires = receipt.get("observed_unix_ms"), receipt.get("expires_unix_ms")
        if type(observed) is not int or type(expires) is not int or not (challenge["issued_unix_ms"] <= observed <= now_ms() <= expires <= observed + 30_000):
            raise BridgeError("stale admission receipt")
        ns = receipt.get("namespace", {})
        if ns.get("private_id") != self.worker["network_namespace"] or ns.get("host_id") in (None, ns.get("private_id")):
            raise BridgeError("namespace is not separately admitted")
        for key in ("loopback_only", "no_nonloopback_routes", "no_forwarding", "no_shared_network"):
            if ns.get(key) is not True:
                raise BridgeError("namespace admission missing " + key)
        if receipt.get("runtime_sha256") != BINARY_SHA256:
            raise BridgeError("admission runtime hash mismatch")
        if not isinstance(receipt.get("evidence_path"), str) or not receipt["evidence_path"]:
            raise BridgeError("independent evidence reference missing")
        # Root owns receipt authenticity through the private stdin channel. Never
        # discover or synthesize an admission from the worker's own kernel reads.
        self.used_receipts.add(receipt_id)
        return receipt

    def scope(self, binding):
        if not isinstance(binding, dict) or set(binding) != {"version", "binding", "request_id", "sequence", "deadline_ms", "width", "height"}:
            raise BridgeError("strict versioned FrameRequest DTO required")
        canonical = binding
        if type(canonical["version"]) is not int or canonical["version"] != 1:
            raise BridgeError("unsupported FrameRequest version")
        if not isinstance(canonical["binding"], dict) or set(canonical["binding"]) != {"scope", "engine_incarnation", "geometry_revision", "surface_epoch"}:
            raise BridgeError("strict FrameBinding DTO required")
        binding = {**canonical["binding"], "renderer_epoch": canonical["binding"]["surface_epoch"],
                   "sequence": canonical["sequence"], "request_id": canonical["request_id"],
                   "deadline_unix_ms": canonical["deadline_ms"],
                   "expected_width": canonical["width"], "expected_height": canonical["height"]}
        if not isinstance(binding, dict) or not isinstance(binding.get("scope"), dict):
            raise BridgeError("complete content Scope missing")
        scope = binding["scope"]
        if set(scope) != set(SCOPE_KEYS) or any(scope[key] is None for key in SCOPE_KEYS):
            raise BridgeError("Scope must bind all eight canonical fields")
        if not isinstance(scope["identity"], dict) or set(scope["identity"]) != {"lineage", "incarnation"}:
            raise BridgeError("invalid canonical Scope identity")
        for token in (*scope["identity"].values(), scope["session"]):
            if not isinstance(token, str) or not token or len(token) > 128:
                raise BridgeError("invalid canonical Scope token")
        if not isinstance(scope["target"], dict) or set(scope["target"]) != {"kind", "id"} or scope["target"]["kind"] not in ("view", "frame_background", "workspace_background"):
            raise BridgeError("invalid canonical Scope target")
        for number in (scope["provider"], scope["resource"], scope["target"]["id"], scope["connection"], scope["instance"], scope["mount"]):
            positive(number, "canonical Scope ID", 2**64 - 2)
        for key in ("geometry_revision", "renderer_epoch", "engine_incarnation", "sequence"):
            positive(binding.get(key), key, 2**64 - 1)
        positive(binding.get("expected_width"), "expected width", self.max_side)
        positive(binding.get("expected_height"), "expected height", self.max_side)
        if binding["expected_width"] * binding["expected_height"] * 4 > self.max_raw:
            raise BridgeError("expected RGBA bound exceeded")
        positive(binding.get("request_id"), "binding request id")
        deadline = binding.get("deadline_unix_ms")
        if type(deadline) is not int or not (now_ms() < deadline <= now_ms() + 15_000):
            raise BridgeError("expired or unbounded request deadline")
        return binding

    def bound_engine(self, request, allow_resize=False):
        binding = self.scope(request.get("request"))
        self.response_request = request["request"]
        key = request.get("view_key")
        engine = self.engines.get(key)
        if not engine or engine["state"] not in ("visible", "hidden"):
            raise BridgeError("provider unavailable; retained pixels/placeholder remain")
        current = proc_record(engine["owned"]["pid"])
        if current is None or identity(current) != engine["owned"]:
            engine["state"] = "lost"
            raise BridgeError("owned engine lost; last good pixels retained")
        if binding["scope"] != engine["scope"] or binding["engine_incarnation"] != engine["incarnation"]:
            raise BridgeError("wrong Scope/engine incarnation")
        capture = request.get("op") == "capture"
        request_key = (key, binding["request_id"])
        if capture and (binding["sequence"] <= engine["sequence"] or request_key in self.seen_requests):
            raise BridgeError("stale/replayed frame request sequence")
        if binding["renderer_epoch"] < engine["renderer_epoch"] or binding["geometry_revision"] < engine["geometry_revision"]:
            raise BridgeError("superseded renderer/geometry binding")
        if not allow_resize and (binding["expected_width"], binding["expected_height"]) != engine["size"]:
            raise BridgeError("wrong expected physical dimensions")
        if not allow_resize and binding["geometry_revision"] != engine["geometry_revision"]:
            raise BridgeError("geometry revision needs explicit resize/bind before response")
        engine["renderer_epoch"] = binding["renderer_epoch"]
        if capture and engine["state"] == "visible" and engine["dirty"]:
            if len(self.seen_requests) >= self.max_request_history:
                raise BridgeError("bounded frame request history exhausted")
            engine["sequence"] = binding["sequence"]
            self.seen_requests.add(request_key)
        return engine, binding

    def timeout(self, binding=None, cap=15):
        remaining = self.scenario_end - time.monotonic()
        if binding:
            remaining = min(remaining, (binding["deadline_unix_ms"] - now_ms()) / 1000)
        if remaining <= 0:
            raise BridgeError("request deadline expired")
        return min(cap, remaining)

    def http(self, engine, method, route, body=None, binding=None, limit=None, operation_timeout=15):
        self.check_time()
        operation_end = min(self.scenario_end, time.monotonic() + self.timeout(binding, operation_timeout))
        conn = http.client.HTTPConnection("127.0.0.1", engine["port"], timeout=self.timeout(binding, operation_timeout))
        data = None if body is None else json.dumps(body, allow_nan=False).encode()
        try:
            conn.request(method, route, data, {"Content-Type": "application/json"})
            response = conn.getresponse()
            bound = limit or self.max_message
            response_end = operation_end
            payload = bytearray()
            if response.getheader("Content-Length") and int(response.getheader("Content-Length")) > bound:
                raise BridgeError("WebDriver Content-Length exceeds operation bound")
            while len(payload) <= bound:
                remaining = response_end - time.monotonic()
                if remaining <= 0:
                    raise BridgeError("WebDriver absolute request deadline exceeded")
                if conn.sock is not None:
                    conn.sock.settimeout(remaining)
                chunk = response.read1(min(65536, bound + 1 - len(payload)))
                if not chunk:
                    break
                payload.extend(chunk)
            if len(payload) > bound:
                raise BridgeError("WebDriver response exceeds operation bound")
            result = json.loads(payload)
            value = result.get("value")
            if response.status >= 400 or (isinstance(value, dict) and "error" in value):
                raise BridgeError("WebDriver causal error: " + str(value)[:1000])
            if binding and now_ms() > binding["deadline_unix_ms"]:
                raise BridgeError("late response discarded; last good pixels retained")
            return value
        finally:
            conn.close()

    def route(self, engine, suffix):
        if not engine.get("session_id"):
            raise BridgeError("listener not independently admitted/session not ready")
        return "/session/" + engine["session_id"] + suffix

    def launch(self, request):
        key = request.get("view_key")
        if not isinstance(key, str) or not key or any(c not in "abcdefghijklmnopqrstuvwxyz0123456789-_" for c in key):
            raise BridgeError("invalid view key")
        existing = self.engines.get(key)
        if existing and existing["state"] != "unloaded":
            raise BridgeError("reload requires independent absence reconciliation first")
        if sum(e["state"] != "unloaded" for e in self.engines.values()) >= self.max_engines:
            raise BridgeError("live engine policy cap exceeded")
        if len(self.engines) >= self.max_frames and not existing:
            raise BridgeError("host frame policy cap exceeded")
        binding = self.scope(request.get("request"))
        self.response_request = request["request"]
        if self.challenge is None or self.challenge["view_key"] not in (None, key):
            raise BridgeError("fresh PRELAUNCH required")
        admitted = self.admission(request.get("receipt"), self.challenge)
        challenge = self.challenge
        self.challenge = None  # One authorization cannot launch another process.
        if file_digest(self.runtime) != BINARY_SHA256:
            raise BridgeError("runtime bytes differ from pinned admitted executable")
        if existing and binding["engine_incarnation"] <= existing["incarnation"]:
            raise BridgeError("reload requires fresh engine incarnation")
        page = (self.fixtures / request["page_reference"]).resolve()
        if page.parent != self.fixtures or page.name not in ("browser-a.html", "browser-b.html") or not page.is_file():
            raise BridgeError("page reference is not a declared local reloadable fixture")
        port = positive(request.get("port"), "WebDriver port", 65535)
        if port < 1024 or any(e["port"] == port and e["state"] != "unloaded" for e in self.engines.values()):
            raise BridgeError("reserved/duplicate engine port")
        size = binding["expected_width"], binding["expected_height"]
        prefix = self.output / (key + "-engine-" + str(binding["engine_incarnation"]))
        log = prefix.with_suffix(".log").open("wb")
        argv = [str(self.runtime), "--headless", "--webdriver=" + str(port),
                "--window-size=" + str(size[0]) + "x" + str(size[1]),
                "--device-pixel-ratio=1", "--temporary-storage", "--config-dir=" + str(prefix) + "-config", page.as_uri()]
        env = os.environ.copy()
        env.update(self.config.get("child_environment", {}))
        try:
            process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                                       env=env, start_new_session=True)
        finally:
            log.close()
        owned = identity(proc_record(process.pid))
        if owned["network_namespace"] != self.worker["network_namespace"]:
            raise BridgeError("child changed namespace; independent root reconciliation required")
        engine = {"process": process, "owned": owned, "port": port, "scope": binding["scope"],
                  "incarnation": binding["engine_incarnation"], "sequence": 0,
                  "geometry_revision": binding["geometry_revision"], "renderer_epoch": binding["renderer_epoch"],
                  "size": size, "page_reference": page.name, "state": "starting", "dirty": True,
                  "pending": False, "captures_requested": 0, "captures_completed": 0, "last_frame": None,
                  "keep_alive": bool(request.get("keep_alive", False)), "document_dirty": False,
                  "reloadable": True, "descendants": {}, "listener_admitted": False,
                  "launch_request": request.get("request")}
        self.engines[key] = engine
        self.emit("LAUNCH", view_key=key, binding=binding, engine=owned, port=port,
                  request=request.get("request"), launch_request_id=challenge["request_id"], admission_receipt_id=admitted["receipt_id"], argv=argv)
        end = min(self.scenario_end, time.monotonic() + 30)
        while time.monotonic() < end:
            if process.poll() is not None:
                engine["state"] = "lost"
                raise BridgeError("engine exited during startup; inspect owned engine log")
            # Kernel listener readiness without contacting WebDriver. Root reads
            # ownership/isolation independently and probes from outside namespace.
            for table in ("tcp", "tcp6"):
                lines = Path("/proc/self/net/" + table).read_text().splitlines()[1:]
                if any(int(line.split()[1].split(":")[1], 16) == port and line.split()[3] == "0A" for line in lines):
                    engine["listener_request"] = uuid.uuid4().hex
                    engine["listener_issued"] = now_ms()
                    return self.emit("LISTENER_READY", view_key=key, binding=binding, engine=owned, port=port,
                                     request_id=engine["listener_request"], stop=True,
                                     required_receipt="independent-root listener ownership plus outside-host denial")
            time.sleep(min(0.05, max(0, end - time.monotonic())))
        engine["state"] = "unknown"
        raise BridgeError("startup30s exceeded; reconcile independent process proof before retry")

    def listener_admit(self, request):
        engine = self.engines.get(request.get("view_key"))
        if not engine or engine["state"] != "starting":
            raise BridgeError("no pending listener admission")
        receipt = request.get("receipt", {})
        challenge = {"request_id": engine["listener_request"], "issued_unix_ms": engine["listener_issued"]}
        self.admission(receipt, challenge)
        if receipt.get("engine") != engine["owned"] or receipt.get("port") != engine["port"]:
            raise BridgeError("listener ownership receipt mismatch")
        if receipt.get("listener_owned") is not True or receipt.get("outside_host_inaccessible") is not True:
            raise BridgeError("independent listener/outside-host proof missing")
        engine["listener_admitted"] = True
        result = self.http(engine, "POST", "/session", {"capabilities": {"alwaysMatch": {"browserName": "servo"}}})
        engine["session_id"] = result["sessionId"]
        self.http(engine, "POST", self.route(engine, "/timeouts"), {"script": 15000, "pageLoad": 15000, "implicit": 0})
        engine["state"] = "visible"
        return self.emit("ENGINE_READY", view_key=request["view_key"], engine=engine["owned"],
                         request=engine["launch_request"],
                         incarnation=engine["incarnation"], actual_capabilities=result.get("capabilities"),
                         restoration="PageReload", captures_completed=0)

    def dom(self, engine, binding):
        return self.http(engine, "POST", self.route(engine, "/execute/sync"),
                         {"script": DOM_SCRIPT, "args": []}, binding)

    def capture(self, request):
        engine, binding = self.bound_engine(request)
        if engine["state"] == "hidden" or not engine["dirty"]:
            return self.emit("NO_CAPTURE", view_key=request["view_key"], binding=binding,
                             state=engine["state"], captures_requested=engine["captures_requested"],
                             captures_completed=engine["captures_completed"], last_frame=engine["last_frame"])
        if engine["pending"]:
            raise BridgeError("one capture already pending; dirty request coalesced")
        engine["pending"] = True
        engine["captures_requested"] += 1
        try:
            dom = self.dom(engine, binding)
            if (dom.get("width"), dom.get("height")) != engine["size"] or dom.get("dpr") != 1:
                raise BridgeError("actual DOM physical dimensions do not match binding")
            encoded = self.http(engine, "GET", self.route(engine, "/screenshot"), binding=binding,
                                limit=4 * ((self.max_png + 2) // 3) + 1024)
            if not isinstance(encoded, str) or len(encoded) > 4 * ((self.max_png + 2) // 3):
                raise BridgeError("encoded PNG bound exceeded")
            png = base64.b64decode(encoded, validate=True)
            if len(png) > self.max_png:
                raise BridgeError("PNG16MiB bound exceeded")
            width, height = png_dimensions(png, self.max_side, self.max_raw)
            if (width, height) != engine["size"] or now_ms() > binding["deadline_unix_ms"]:
                raise BridgeError("stale/wrong-size engine screenshot discarded")
            sha = digest(png)
            stem = request["view_key"] + "-" + str(engine["incarnation"]) + "-" + str(binding["sequence"])
            path = self.output / (stem + ".png")
            path.write_bytes(png)
            frame = {"view_key": request["view_key"], "png_path": path.name, "png_sha256": sha,
                     "encoded_bytes": len(png), "width": width, "height": height,
                     "row_stride": width * 4, "pixel_format": "SRGBA8", "alpha": "straight", "origin": "top-left",
                     "binding": binding, "engine": engine["owned"], "dom": dom,
                     "engine_receipt_unix_ms": now_ms(), "receipt_path": stem + "-receipt.json",
                     "engine_source_commit": SERVO_COMMIT, "runtime_sha256": BINARY_SHA256}
            frame["request"] = request.get("request")
            frame["case_name"] = request.get("case_name", "normal")
            frame["issued_ms"] = binding["deadline_unix_ms"] - 15_000
            frame["received_ms"] = now_ms()
            frame["request_sha256"] = digest(json.dumps(request["request"], sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False).encode())
            if frame["received_ms"] >= binding["deadline_unix_ms"]:
                raise BridgeError("capture completion missed exact request deadline")
            (self.output / frame["receipt_path"]).write_text(json.dumps(frame, indent=2) + "\n")
            engine["last_frame"], engine["dirty"] = frame, False
            self.captured_frames.append(frame)
            engine["captures_completed"] += 1
            return self.emit("FRAME", **frame, captures_requested=engine["captures_requested"],
                             captures_completed=engine["captures_completed"])
        finally:
            engine["pending"] = False

    def resize(self, request):
        engine, binding = self.bound_engine(request, allow_resize=True)
        if binding["geometry_revision"] <= engine["geometry_revision"]:
            raise BridgeError("resize requires new canonical geometry revision")
        width, height = binding["expected_width"], binding["expected_height"]
        actual = self.http(engine, "POST", self.route(engine, "/window/rect"), {"width": width, "height": height}, binding)
        dom = self.dom(engine, binding)
        if (dom.get("width"), dom.get("height")) != (width, height):
            raise BridgeError("resize failed; previous valid pixels retained")
        engine["size"] = width, height
        engine["geometry_revision"], engine["dirty"] = binding["geometry_revision"], True
        return self.emit("RESIZED", view_key=request["view_key"], binding=binding, engine=engine["owned"], dom=dom, rect=actual)

    def input(self, request):
        engine, binding = self.bound_engine(request)
        x, y = request.get("x"), request.get("y")
        if type(x) is not int or type(y) is not int or not (0 <= x < engine["size"][0] and 0 <= y < engine["size"][1]):
            raise BridgeError("input must already be mapped once to local physical coordinates")
        text = request.get("text", "")
        if not isinstance(text, str) or len(text) > 256:
            raise BridgeError("keyboard payload invalid")
        actions = [{"type": "pointer", "id": "host-pointer", "parameters": {"pointerType": "mouse"},
                    "actions": [{"type": "pointerMove", "duration": 0, "origin": "viewport", "x": x, "y": y},
                                {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}]}]
        before = self.dom(engine, binding)
        self.http(engine, "POST", self.route(engine, "/actions"), {"actions": actions}, binding)
        # Separate sources in one request share ticks; finish the click before typing.
        if text:
            keyboard = [{"type": "key", "id": "host-keyboard", "actions":
                         [item for char in text for item in ({"type": "keyDown", "value": char}, {"type": "keyUp", "value": char})]}]
            self.http(engine, "POST", self.route(engine, "/actions"), {"actions": keyboard}, binding)
        self.http(engine, "DELETE", self.route(engine, "/actions"), binding=binding)
        after = self.dom(engine, binding)
        engine["dirty"] = True
        if request.get("document_dirty") is True:
            engine["document_dirty"] = True
        return self.emit("INPUT", view_key=request["view_key"], binding=binding, engine=engine["owned"],
                         local_physical={"x": x, "y": y}, before=before, after=after,
                         route="genuine WebDriver PerformActions pointer/keyboard; no synthetic DOM event dispatch")

    def mutate(self, request):
        engine, binding = self.bound_engine(request)
        result = self.http(engine, "POST", self.route(engine, "/execute/sync"),
                           {"script": "return window.kumihimoMutate(arguments[0]);", "args": [request.get("value", "dirty")]}, binding)
        engine["dirty"] = True
        return self.emit("DIRTY", view_key=request["view_key"], binding=binding, engine=engine["owned"], result=result)

    def inspect(self, request):
        engine, binding = self.bound_engine(request)
        return self.emit("OBSERVATION", view_key=request["view_key"], binding=binding,
                         engine=engine["owned"], resources=proc_record(engine["owned"]["pid"]),
                         state=engine["state"], dirty=engine["dirty"], dom=self.dom(engine, binding),
                         captures_requested=engine["captures_requested"], captures_completed=engine["captures_completed"])

    def visibility(self, request):
        engine, binding = self.bound_engine(request)
        visible = request.get("visible")
        if type(visible) is not bool:
            raise BridgeError("visibility flag missing")
        engine["state"] = "visible" if visible else "hidden"
        return self.emit("VISIBILITY", view_key=request["view_key"], binding=binding, state=engine["state"],
                         resources=proc_record(engine["owned"]["pid"]), suspension_proven=False, memory_release_proven=False)

    def owned_descendants(self, engine):
        records = {}
        known_owned = {self.worker["pid"]}
        for owned_engine in self.engines.values():
            known_owned.add(owned_engine["owned"]["pid"])
            known_owned.update(owned_engine["descendants"])
        for entry in Path("/proc").iterdir():
            if entry.name.isdigit():
                pid = int(entry.name)
                try:
                    record = proc_record(pid)
                except PermissionError:
                    if pid in known_owned:
                        raise
                    continue  # Foreign metadata is not owned-process absence proof.
                if record:
                    records[record["pid"]] = record
        ancestors = {engine["owned"]["pid"]}
        for _ in range(len(records)):
            found = {pid for pid, record in records.items() if record["ppid"] in ancestors}
            if found <= ancestors:
                break
            ancestors |= found
        for pid in ancestors - {engine["owned"]["pid"]}:
            engine["descendants"][pid] = identity(records[pid])
        return list(engine["descendants"].values())

    def unload(self, request):
        engine, binding = self.bound_engine(request)
        if engine["keep_alive"] or engine["document_dirty"] or not engine["reloadable"]:
            return self.emit("UNLOAD_PROTECTED", view_key=request["view_key"], binding=binding, engine=engine["owned"],
                             keep_alive=engine["keep_alive"], document_dirty=engine["document_dirty"], content_retained=True)
        descendants = self.owned_descendants(engine)
        engine["state"] = "shutdown_requested"
        grace_end = min(self.scenario_end, time.monotonic() + 10)
        try:
            self.http(engine, "DELETE", self.route(engine, "/servo/shutdown"), binding=binding, operation_timeout=10)
        except (BridgeError, OSError, http.client.HTTPException):
            pass  # The independent absence receipt reconciles the outcome.
        while engine["process"].poll() is None and time.monotonic() < grace_end:
            self.owned_descendants(engine)
            time.sleep(0.05)
        for owned in [*engine["descendants"].values(), engine["owned"]]:
            current = proc_record(owned["pid"])
            if current and identity(current) == owned:
                os.kill(owned["pid"], signal.SIGTERM)
        end = min(self.scenario_end, time.monotonic() + 1)
        while engine["process"].poll() is None and time.monotonic() < end:
            time.sleep(0.05)
        for owned in [*engine["descendants"].values(), engine["owned"]]:
            current = proc_record(owned["pid"])
            if current and identity(current) == owned and current["state"] != "Z":
                os.kill(owned["pid"], signal.SIGKILL)
        engine["process"].poll()  # Reap if exited; no worker claim of absence.
        engine["state"] = "unknown"
        engine["reconcile_request"] = uuid.uuid4().hex
        engine["reconcile_issued"] = now_ms()
        if request["view_key"] == "browser-a" and engine["incarnation"] == 1:
            self.unload_request_id = engine["reconcile_request"]
        return self.emit("UNLOAD_UNKNOWN", view_key=request["view_key"], binding=binding,
                         engine=engine["owned"], descendants=list(engine["descendants"].values()),
                         request_id=engine["reconcile_request"], stop=True, content_retained=True,
                         placeholder="Reload declared page", required_receipt="independent-root exact PID/starttime absence and no live descendants")

    def reconcile(self, request):
        engine = self.engines.get(request.get("view_key"))
        receipt = request.get("receipt", {})
        if not engine or engine["state"] != "unknown":
            raise BridgeError("no unknown lifecycle outcome to reconcile")
        if receipt.get("issuer") != "independent-root" or receipt.get("worker") != self.worker or receipt.get("engine") != engine["owned"]:
            raise BridgeError("independent absence ownership mismatch")
        if receipt.get("request_id") != engine.get("reconcile_request") or receipt.get("absent") is not True or receipt.get("no_live_descendants") is not True:
            raise BridgeError("independent absence/no descendants not proven; retry blocked")
        if receipt.get("descendants") != list(engine["descendants"].values()):
            raise BridgeError("independent descendant set mismatch")
        if type(receipt.get("observed_unix_ms")) is not int or not engine["reconcile_issued"] <= receipt["observed_unix_ms"] <= now_ms():
            raise BridgeError("stale independent absence receipt")
        if not receipt.get("evidence_path") or not receipt.get("receipt_id"):
            raise BridgeError("independent absence evidence missing")
        engine["state"] = "unloaded"
        return self.emit("CONFIRMED_UNLOADED", view_key=request["view_key"], engine=engine["owned"],
                         independent_receipt=receipt, logical_scope=engine["scope"], placeholder="Reload declared page",
                         restoration="PageReload; arbitrary JS/media state unsupported")

    def dispatch(self, request):
        self.check_time()
        if len(self.events) >= self.max_events - 32 and request.get("op") not in ("policy", "unload", "reconcile"):
            raise BridgeError("event history budget exhausted before operation; root cleanup required")
        allowed = {"protocol", "op", "view_key", "request", "receipt", "page_reference", "port", "keep_alive",
                   "case_name", "x", "y", "text", "value", "document_dirty", "visible"}
        if not isinstance(request, dict) or not set(request) <= allowed:
            raise BridgeError("unknown versioned operation fields")
        self.response_request = request.get("request")
        if request.get("protocol") != PROTOCOL:
            raise BridgeError("unsupported protocol version")
        op = request.get("op")
        if op == "prepare_launch":
            return self.prepare_launch(request.get("view_key"))
        if op == "launch":
            return self.launch(request)
        if op == "listener_admit":
            return self.listener_admit(request)
        if op == "reconcile":
            return self.reconcile(request)
        if op == "policy":
            engine, binding = self.bound_engine(request)
            for field in ("keep_alive", "document_dirty"):
                if type(request.get(field)) is not bool:
                    raise BridgeError("explicit host policy boolean required: " + field)
            engine["keep_alive"] = request["keep_alive"]
            engine["document_dirty"] = request["document_dirty"]
            return self.emit("POLICY", view_key=request["view_key"], binding=binding,
                             keep_alive=engine["keep_alive"], document_dirty=engine["document_dirty"])
        if op in ("freeze", "throttle", "frame_ready", "popout_transfer"):
            engine, binding = self.bound_engine(request)
            return self.emit("UNSUPPORTED", view_key=request["view_key"], binding=binding, operation=op,
                             reason="external screenshot bridge has no engine-context transfer/freeze/throttle/frame-ready support",
                             content_retained=True, confirmed_state=engine["state"])
        routes = {"capture": self.capture, "resize": self.resize, "input": self.input, "mutate": self.mutate,
                  "inspect": self.inspect, "visibility": self.visibility, "unload": self.unload}
        if op not in routes:
            raise BridgeError("unknown operation")
        return routes[op](request)

    def read_request(self):
        while b"\n" not in self.input_buffer:
            wait = max(0, self.scenario_end - time.monotonic())
            if not select.select([sys.stdin.fileno()], [], [], wait)[0]:
                raise BridgeError("scenario deadline while waiting for independent root")
            block = os.read(sys.stdin.fileno(), min(65536, self.max_message + 1 - len(self.input_buffer)))
            if not block:
                if self.input_buffer:
                    raise BridgeError("truncated JSON line")
                return None
            self.input_buffer.extend(block)
            if len(self.input_buffer) > self.max_message and b"\n" not in self.input_buffer:
                raise BridgeError("bounded JSON line exceeded")
        line, _, tail = self.input_buffer.partition(b"\n")
        self.input_buffer = bytearray(tail)
        if len(line) > self.max_message:
            raise BridgeError("bounded JSON line exceeded")
        result = json.loads(line)
        if not isinstance(result, dict):
            raise BridgeError("JSON request must be object")
        return result

    def save_diagnostics(self):
        manifest = {"protocol": "kumihimo.servo-batch.v1", "worker": self.worker,
                    "engine_source_commit": SERVO_COMMIT, "runtime_sha256": BINARY_SHA256,
                    "unload_request_id": self.unload_request_id,
                    "browsers": self.captured_frames,
                    "events": self.events, "runtime_verdict": "OBSERVATIONS_ONLY_REQUIRES_INDEPENDENT_ROOT_AND_GPU_EVALUATION",
                    "capabilities": {"restoration": "PageReload", "freeze": "Unsupported", "throttle": "Unsupported",
                                     "frame_ready": "Unsupported", "popout_transfer": "Unsupported"}}
        path = self.output / "servo-manifest.json"
        encoded = (json.dumps(manifest, separators=(",", ":"), allow_nan=False) + "\n").encode()
        if len(encoded) > self.max_message:
            raise BridgeError("aggregate manifest exceeds4MiB; preserve bounded source artifacts")
        path.write_bytes(encoded)
        return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True, help="ignored local JSON: runtime_executable/output_root/fixture_root/child_environment")
    args = parser.parse_args()
    config_path = Path(args.config).resolve()
    if config_path.stat().st_size > 4 * 1024 * 1024:
        raise BridgeError("config exceeds cap")
    worker = Worker(json.loads(config_path.read_text()))
    try:
        while True:
            request = worker.read_request()
            if request is None:
                break
            try:
                worker.dispatch(request)
            except (BridgeError, OSError, ValueError, KeyError, http.client.HTTPException) as error:
                worker.emit("ERROR", request_id=request.get("binding", {}).get("request_id"),
                            view_key=request.get("view_key"), binding=request.get("binding"),
                            reason=str(error)[:1500], content_retained=True, retry="independent reconciliation/changed evidence required; no automatic retry")
    finally:
        worker.save_diagnostics()
        # EOF/deadline never authorizes an unowned kill or a false cleanup claim.
        # Root must drive eligible unload + independent reconcile before closing.
        live = [key for key, engine in worker.engines.items() if engine["state"] != "unloaded"]
        worker.emit("STOP", live_views=live, cleanup_confirmed=not live,
                    independent_cleanup_required=bool(live), manifest_path="servo-manifest.json")


if __name__ == "__main__":
    main()
