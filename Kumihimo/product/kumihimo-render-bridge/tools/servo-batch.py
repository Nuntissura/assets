#!/usr/bin/env python3
"""Bounded actual-engine scenario driven through independent root-owned stdio.

ROOT runs --config LOCAL.json --plan compose-plan.json inside its private netns.
At PRELAUNCH answer {protocol,op:'admit',receipt:<fresh independent-root>}.
At LISTENER_READY answer {protocol,op:'listener_admit',view_key,receipt:<root>}.
At CAPTURE_READY answer {protocol,op:'capture',view_key,case_name,
 capture_challenge,request:<immutable fresh root-issued FrameRequest>}.
At UNLOAD_UNKNOWN answer {protocol,op:'reconcile',view_key,receipt:<root>}.
The batch performs no admission/kernel proof itself; it never emits a PASS for
the seven evaluator methods. Linux engine PNG receipts must be transferred
byte-for-byte with servo-manifest.json to the Windows Rust compose example.
"""
import argparse
import copy
import importlib.util
import json
import math
from pathlib import Path
import time
import uuid

spec = importlib.util.spec_from_file_location("servo_worker", Path(__file__).with_name("servo-worker.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
Worker, BridgeError, PROTOCOL, now_ms = module.Worker, module.BridgeError, module.PROTOCOL, module.now_ms


class Batch:
    def __init__(self, worker, plan):
        self.worker = worker
        if plan.get("protocol") != "kumihimo.compose-plan.v1":
            raise BridgeError("canonical compose plan version missing")
        self.plan = plan
        self.requests = {}
        self.diagnostic_id = 1_000_000
        self.observations = []
        cases = plan.get("cases", [])
        if [case.get("name") for case in cases] != ["normal", "narrow", "scale-1-5", "moved-resized"]:
            raise BridgeError("all four authoritative compose scenarios required")
        for case in cases:
            if {row.get("view_key") for row in case.get("browsers", [])} != {"browser-a", "browser-b"}:
                raise BridgeError("each compose case must bind both canonical browser views")
            for row in case["browsers"]:
                worker.scope(self.fresh(row["intent"]))

    def fresh(self, request):
        result = copy.deepcopy(request)
        result["deadline_ms"] = now_ms() + 15_000
        return result

    def command(self, key, op, request=None, **kwargs):
        current = self.fresh(request or self.requests[key])
        result = self.worker.dispatch({"protocol": PROTOCOL, "op": op, "view_key": key,
                                       "request": current, **kwargs})
        self.observations.append(result)
        return result

    def answer(self, op, key=None):
        request = self.worker.read_request()
        if request is None:
            raise BridgeError("root closed independent authorization channel")
        if request.get("protocol") != PROTOCOL or request.get("op") != op or (key is not None and request.get("view_key") != key):
            raise BridgeError("root response does not match pending admission/reconciliation")
        return request

    def launch(self, row, port, keep_alive=False, restore=False):
        key = row["view_key"]
        if self.worker.challenge is None:
            self.worker.prepare_launch(key)
        admission = self.answer("admit")
        request = self.fresh(row["intent"])
        self.worker.launch({"protocol": PROTOCOL, "op": "launch", "view_key": key,
                            "request": request, "receipt": admission.get("receipt"), "port": port,
                            "page_reference": key + ".html", "keep_alive": keep_alive})
        ready = self.answer("listener_admit", key)
        self.worker.listener_admit(ready)
        self.requests[key] = request
        return self.command(key, "inspect")

    def diagnostic_request(self, key):
        request = self.fresh(self.requests[key])
        self.diagnostic_id += 1
        request["request_id"] = self.diagnostic_id
        request["sequence"] = max(self.worker.engines[key]["sequence"] + 1, self.diagnostic_id)
        return request

    def require(self, condition, reason):
        if not condition:
            raise BridgeError("actual-engine scenario finding: " + reason)

    def capture(self, key, case_name, request=None):
        intent = copy.deepcopy(request or self.requests[key])
        intent.pop("deadline_ms", None)
        challenge = uuid.uuid4().hex
        self.worker.emit("CAPTURE_READY", view_key=key, case_name=case_name,
                         capture_challenge=challenge, intent=intent, stop=True,
                         required_request="independent ROOT issues immutable FrameRequest with deadline_ms=UTC issue+15000")
        answer = self.answer("capture", key)
        actual = answer.get("request")
        if not isinstance(actual, dict):
            raise BridgeError("root capture request missing")
        comparison = copy.deepcopy(actual)
        comparison.pop("deadline_ms", None)
        if answer.get("capture_challenge") != challenge or answer.get("case_name") != case_name or comparison != intent:
            raise BridgeError("root capture challenge/intent mismatch")
        self.worker.scope(actual)
        response = self.worker.dispatch({"protocol": PROTOCOL, "op": "capture", "view_key": key,
                                         "request": actual, "case_name": case_name})
        self.observations.append(response)
        return response

    def input_each_view(self, case):
        rows = {row["view_key"]: row for row in case["browsers"]}
        for key, peer in (("browser-a", "browser-b"), ("browser-b", "browser-a")):
            row = rows[key]
            scale = case["scale"]
            slot = row.get("slot_dip")
            clip = row.get("clip_dip")
            if not isinstance(slot, dict) or not isinstance(clip, dict) or scale <= 0:
                raise BridgeError("canonical slot_dip/clip_dip/scale required for input recipient proof")
            def mapped(rect):
                local = {"x": (rect["x"] + rect["width"] / 2) / scale,
                         "y": (rect["y"] + rect["height"] / 2) / scale}
                global_point = {"x": slot["x"] + local["x"], "y": slot["y"] + local["y"]}
                self.require(0 <= local["x"] < slot["width"] and 0 <= local["y"] < slot["height"],
                             "local DIP target outside canonical RenderSlot")
                self.require(clip["x"] <= global_point["x"] < clip["x"] + clip["width"] and
                             clip["y"] <= global_point["y"] < clip["y"] + clip["height"],
                             "global DIP target outside canonical ancestor clip")
                x, y = math.floor(local["x"] * scale + 0.5), math.floor(local["y"] * scale + 0.5)
                self.require(0 <= x < row["intent"]["width"] and 0 <= y < row["intent"]["height"],
                             "rounded engine target outside canonical physical dimensions")
                mapping = {"local_dip": local, "global_dip": global_point, "scale": scale,
                           "expected_engine_css": {"x": x, "y": y}, "engine_dpr": 1,
                           "slot_dip": slot, "clip_dip": clip, "source": "canonical compose-plan RenderSlot; mapped once"}
                return x, y, mapping
            unchanged = self.command(peer, "inspect")["dom"]
            before = self.command(key, "inspect")["dom"]
            button = before["button"]
            x, y, mapping = mapped(button)
            click = self.command(key, "input", x=x, y=y)
            self.worker.emit("INPUT_MAPPING", view_key=key, case_name=case["name"], mapping=mapping,
                             actual_dom_client_css=click["after"]["state"]["lastPointer"], engine=click["engine"],
                             request=click["request"], host_dispatch_claim="mapping receipt only; actual Rust dispatcher evaluated separately")
            self.require(click["after"]["state"]["clicks"] == before["state"]["clicks"] + 1,
                         key + " real pointer did not reach expected button")
            self.require(click["after"]["state"]["trustedPointer"] is True,
                         key + " actual engine reports untrusted pointer")
            self.require(click["after"]["state"]["lastPointer"] == {"x": x, "y": y},
                         key + " DOM client coordinates differ from mapped local physical pixels")
            field = click["after"]["field"]
            x, y, mapping = mapped(field)
            text = "typed-a" if key == "browser-a" else "typed-b"
            typed = self.command(key, "input", x=x, y=y, text=text)
            self.worker.emit("INPUT_MAPPING", view_key=key, case_name=case["name"], mapping=mapping,
                             keyboard_text=text, actual_input=typed["after"]["input"], engine=typed["engine"],
                             request=typed["request"], host_dispatch_claim="mapping receipt only; actual Rust dispatcher evaluated separately")
            self.require(typed["after"]["input"] == before["input"] + text and typed["after"]["state"]["trustedKeyboard"] is True,
                         key + " genuine WebDriver keyboard did not reach its field")
            self.require(self.command(peer, "inspect")["dom"] == unchanged,
                         "addressed " + key + " input changed peer " + peer)

    def static_proof(self):
        counts = {key: self.worker.engines[key]["captures_completed"] for key in self.requests}
        for key in self.requests:
            result = self.capture(key, "static-no-capture", self.diagnostic_request(key))
            self.require(result["event"] == "NO_CAPTURE", "static view produced duplicate screenshot")
            self.require(self.worker.engines[key]["captures_completed"] == counts[key], "static capture count advanced")

    def compose_captures(self):
        for index, case in enumerate(self.plan["cases"]):
            for row in case["browsers"]:
                key, request = row["view_key"], row["intent"]
                if index:
                    engine = self.worker.engines[key]
                    changed = (request["width"], request["height"]) != engine["size"]
                    revision_changed = request["binding"]["geometry_revision"] != engine["geometry_revision"]
                    if changed or revision_changed:
                        peer_before = self.command("browser-b", "inspect") if key == "browser-a" else None
                        peer_hash = self.worker.engines["browser-b"]["last_frame"]["png_sha256"] if peer_before else None
                        peer_count = self.worker.engines["browser-b"]["captures_completed"] if peer_before else None
                        self.command(key, "resize", request=request)
                        if peer_before:
                            peer_after = self.command("browser-b", "inspect")
                            self.require(peer_before["dom"] == peer_after["dom"] and peer_before["engine"] == peer_after["engine"],
                                         "canonical A-only resize changed B DOM/identity")
                            self.require(self.worker.engines["browser-b"]["last_frame"]["png_sha256"] == peer_hash and
                                         self.worker.engines["browser-b"]["captures_completed"] == peer_count,
                                         "canonical A-only resize recaptured/changed B pixels")
                            self.worker.emit("INDEPENDENT_RESIZE_OBSERVED", case_name=case["name"], view_key=key,
                                             request=self.fresh(request), unaffected_engine=peer_after["engine"],
                                             unaffected_dom=peer_after["dom"], unaffected_png_sha256=peer_hash,
                                             unaffected_capture_count=peer_count)
                    else:
                        raise BridgeError("compose plan asks duplicate static screenshot; preserve actual prior receipt instead")
                self.requests[key] = self.fresh(request)
            if case["name"] in ("normal", "scale-1-5"):
                self.input_each_view(case)
            for row in case["browsers"]:
                key, request = row["view_key"], row["intent"]
                result = self.capture(key, case["name"])
                self.require(result["event"] == "FRAME", "compose scenario missing actual screenshot")
                self.require(result["request"]["request_id"] == request["request_id"] and result["request"]["sequence"] == request["sequence"],
                             "canonical compose capture identity changed")
            if index == 0:
                self.static_proof()

    def dirty_hidden_resize_proof(self):
        a, b = "browser-a", "browser-b"
        b_dom = self.command(b, "inspect")["dom"]
        b_frame = self.worker.engines[b]["last_frame"]
        b_count = self.worker.engines[b]["captures_completed"]
        self.command(a, "inspect")
        self.command(b, "inspect")
        time.sleep(min(0.15, self.worker.timeout(cap=0.15)))
        self.command(a, "mutate", value="A-only dirty")
        dirty = self.capture(a, "dirty-a", self.diagnostic_request(a))
        self.require(dirty["event"] == "FRAME", "A-only dirty capture missing")
        unchanged = self.capture(b, "b-unchanged", self.diagnostic_request(b))
        self.require(unchanged["event"] == "NO_CAPTURE" and self.worker.engines[b]["captures_completed"] == b_count,
                     "A mutation recaptured B")
        self.require(self.command(b, "inspect")["dom"] == b_dom and self.worker.engines[b]["last_frame"]["png_sha256"] == b_frame["png_sha256"],
                     "A mutation changed B DOM/pixels")
        self.command(a, "visibility", visible=False)
        self.command(a, "inspect")
        self.command(a, "mutate", value="hidden-host-dirty")
        self.require(self.capture(a, "hidden-no-capture", self.diagnostic_request(a))["event"] == "NO_CAPTURE",
                     "hidden view captured")
        time.sleep(min(0.15, self.worker.timeout(cap=0.15)))
        self.command(a, "inspect")
        for op in ("freeze", "throttle", "frame_ready", "popout_transfer"):
            self.require(self.command(a, op)["event"] == "UNSUPPORTED", "unsupported capability was silently confirmed")
        self.command(a, "visibility", visible=True)
        self.capture(a, "visible-after-hidden", self.diagnostic_request(a))

    def unload_reconcile(self, key):
        response = self.command(key, "unload")
        self.require(response["event"] == "UNLOAD_UNKNOWN", "eligible unload did not require independent reconciliation")
        answer = self.answer("reconcile", key)
        result = self.worker.reconcile(answer)
        self.require(result["event"] == "CONFIRMED_UNLOADED", "independent unload not confirmed")

    def lifecycle_proof(self):
        a, b = "browser-a", "browser-b"
        self.require(self.command(b, "unload")["event"] == "UNLOAD_PROTECTED", "keep-alive unload not protected")
        self.command(a, "policy", keep_alive=False, document_dirty=True)
        self.require(self.command(a, "unload")["event"] == "UNLOAD_PROTECTED", "dirty unload not protected")
        self.command(a, "policy", keep_alive=False, document_dirty=False)
        b_before = self.command(b, "inspect")
        self.unload_reconcile(a)
        b_after = self.command(b, "inspect")
        self.require(b_before["engine"] == b_after["engine"] and b_before["dom"] == b_after["dom"],
                     "A unload changed unaffected live B identity/content")
        reload_row = self.plan.get("lifecycle", {}).get("reload_a")
        if not isinstance(reload_row, dict) or reload_row.get("view_key") != a:
            raise BridgeError("canonical lifecycle.reload_a intent missing")
        restore_request = self.fresh(reload_row["intent"])
        previous = self.worker.engines[a]["owned"]
        self.launch(reload_row, self.worker.engines[a]["port"], restore=True)
        self.require(self.worker.engines[a]["owned"] != previous, "page reload reused old engine identity")
        restored = self.capture(a, "reloaded-a", restore_request)
        self.require(restored["event"] == "FRAME" and restored["dom"]["state"]["clicks"] == 0,
                     "fresh page reload did not return actual declared fixture pixels")
        self.unload_reconcile(a)
        self.command(b, "policy", keep_alive=False, document_dirty=False)
        self.unload_reconcile(b)

    def run(self):
        ports = self.worker.config.get("ports", [7001, 7002])
        if not isinstance(ports, list) or len(ports) != 2 or ports[0] == ports[1]:
            raise BridgeError("two distinct root-configured engine ports required")
        for row, port in zip(self.plan["cases"][0]["browsers"], ports):
            self.launch(row, port, keep_alive=row["view_key"] == "browser-b")
        self.compose_captures()
        self.dirty_hidden_resize_proof()
        self.lifecycle_proof()
        path = self.worker.save_diagnostics()
        self.worker.emit("SCENARIO_OBSERVED", manifest_path=path.name, runtime_verdict="NOT_AN_INDEPENDENT_EVALUATOR_PASS",
                         captures={key: engine["captures_completed"] for key, engine in self.worker.engines.items()},
                         required_remaining="independent root artifact/hash/kernel/resource reconciliation and actual Windows GPU composition/inspection")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", required=True)
    parser.add_argument("--plan", required=True)
    args = parser.parse_args()
    for path in (args.config, args.plan):
        if Path(path).stat().st_size > 4 * 1024 * 1024:
            raise BridgeError("config/plan exceeds bounded JSON input")
    worker = Worker(json.loads(Path(args.config).read_text()))
    try:
        Batch(worker, json.loads(Path(args.plan).read_text())).run()
    except (BridgeError, OSError, ValueError, KeyError) as error:
        worker.emit("SCENARIO_FAILED", reason=str(error)[:1500], retry="none; independent root must reconcile ownership before retry",
                    owned_engines={key: engine["owned"] for key, engine in worker.engines.items()})
        # Leave an observable manual recovery channel for ROOT. No autonomous
        # retries, synthetic pixels or false cleanup. Existing owned engines can
        # be shut down through explicit policy/unload/reconcile requests.
        while time.monotonic() < worker.scenario_end:
            request = worker.read_request()
            if request is None:
                break
            try:
                worker.dispatch(request)
            except (BridgeError, OSError, ValueError, KeyError) as recovery_error:
                worker.emit("ERROR", reason=str(recovery_error)[:1500], content_retained=True)
    finally:
        worker.save_diagnostics()
        live = [key for key, engine in worker.engines.items() if engine["state"] != "unloaded"]
        worker.emit("STOP", live_views=live, cleanup_confirmed=not live, independent_cleanup_required=bool(live),
                    manifest_path="servo-manifest.json")


if __name__ == "__main__":
    main()
