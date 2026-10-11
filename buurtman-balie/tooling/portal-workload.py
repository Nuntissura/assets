"""Measure the selected provisional P1 runner; no Portal endpoint or credentials.

The result/stdin handshake allows a final OS resource read before owned-process exit.
Working-set peak includes seeding and injected faults, not just timed workload phases.
"""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import struct
import subprocess
import threading
import time
import uuid

import yaml

ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / 'repo-governance/specification/current/specification.yaml'


def fingerprints(binary):
    paths = [ROOT / name for name in ['Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
        'tooling/portal-workload.py', 'crates/buurtman-balie-host-demo/Cargo.toml',
        'crates/buurtman-balie-host-demo/examples/portal_workload.rs',
        'repo-governance/work-packets/active/WP-BB-010-swarm-backpressure-v1/wp-contract.yaml']]
    paths += list((ROOT / 'src').glob('*.rs')) + [SPEC, binary]
    return {str(path.relative_to(ROOT)) if path.is_relative_to(ROOT) else str(path):
        hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}


def binary_architecture(binary):
    with binary.open('rb') as stream:
        if stream.read(2) != b'MZ':
            raise ValueError('Expected Windows PE executable')
        stream.seek(0x3c)
        offset = struct.unpack('<I', stream.read(4))[0]
        stream.seek(offset)
        if stream.read(4) != b'PE\0\0':
            raise ValueError('Invalid PE signature')
        return {0xaa64: 'ARM64', 0x8664: 'x64'}[struct.unpack('<H', stream.read(2))[0]]


class MemoryCounters(ctypes.Structure):
    _fields_ = [('cb', wintypes.DWORD), ('PageFaultCount', wintypes.DWORD)] + [
        (name, ctypes.c_size_t) for name in ['PeakWorkingSetSize', 'WorkingSetSize',
        'QuotaPeakPagedPoolUsage', 'QuotaPagedPoolUsage', 'QuotaPeakNonPagedPoolUsage',
        'QuotaNonPagedPoolUsage', 'PagefileUsage', 'PeakPagefileUsage']]


class OwnedResources:
    def __init__(self, child):
        # Windows Popen retains the exact process handle returned by CreateProcess.
        self.handle = wintypes.HANDLE(int(child._handle))
        self.psapi = ctypes.WinDLL('psapi', use_last_error=True)
        self.kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        self.psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE,
            ctypes.POINTER(MemoryCounters), wintypes.DWORD]
        self.psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
        self.kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [
            ctypes.POINTER(wintypes.FILETIME)] * 4
        self.kernel.GetProcessTimes.restype = wintypes.BOOL

    def read(self):
        memory = MemoryCounters()
        memory.cb = ctypes.sizeof(memory)
        if not self.psapi.GetProcessMemoryInfo(self.handle, ctypes.byref(memory), memory.cb):
            raise ctypes.WinError(ctypes.get_last_error())
        times = [wintypes.FILETIME() for _ in range(4)]
        if not self.kernel.GetProcessTimes(self.handle, *(ctypes.byref(item) for item in times)):
            raise ctypes.WinError(ctypes.get_last_error())
        cpu = sum((item.dwHighDateTime << 32) | item.dwLowDateTime for item in times[2:])
        return {'peak_working_set_bytes': memory.PeakWorkingSetSize,
                'working_set_bytes': memory.WorkingSetSize, 'cpu_seconds': cpu / 10_000_000}


def reconcile(payload, profile):
    if payload.get('profile_id') != profile['profile_id'] or payload.get('passed') is not True:
        raise ValueError('Runner profile/invariants failed: ' + str(payload.get('failures')))
    def require(condition, message):
        if not condition:
            raise ValueError(message)
    require(payload['worker_config'] == profile['worker_limits'], 'Worker limits differ from P1')
    require(payload['service_limits'] == {'snapshot_bytes': profile['service_limits']['snapshot_bytes'],
            'operation_count': 20000}, 'Service limits differ from inspected defaults')
    seed = profile['seed']
    baseline = {'records': seed['records'], 'relations': seed['record_links'] + seed['opaque_portal_account_refs'],
                'receipts': 160, 'revision': 160, 'kinds': {'Organization': seed['organizations'],
                'Customer': seed['customers_b2c_b2b'], 'SupportCase': seed['support_cases'], 'Interaction': seed['interactions']}}
    require(payload['seed'] == baseline, 'Seed canonical counts differ from P1')
    for key, expected in [('seed_description_ascii_bytes', seed['description_ascii_bytes']),
                          ('seed_record_links', seed['record_links']),
                          ('seed_opaque_portal_account_refs', seed['opaque_portal_account_refs'])]:
        require(payload[key] == expected, 'Seed metadata differs from P1: ' + key)
    faults = payload['faults']
    require([faults['injected_delay_ms'], faults['queue_deadline_ms'], faults['caller_deadline_ms']] ==
            [profile['fault_phases'][key] for key in ['store_delay_ms', 'queued_deadline_ms', 'caller_deadline_ms']],
            'Fault budgets differ from P1')
    expiry, loss = faults['queue_expiry'], faults['response_loss']
    require(expiry['verified'] is True and expiry['load_outcome'] == 'storage' and
            expiry['queued_cas_outcome'] == 'capacity' and expiry['cas_dispatches'] == 0 and
            expiry['pressure']['expired'] > 0 and expiry['canonical'] ==
            {'records': 0, 'relations': 0, 'receipts': 0, 'revision': 0, 'kinds': {}}, 'Queue-expiry invariant failed')
    require(loss['verified'] is True and loss['initial_outcome'] == 'storage' and
            loss['replay_outcome'] == 'success' and loss['changed_intent'] == 'OperationReused' and
            loss['identical_replay_unchanged'] is True and loss['cas_dispatches'] == 1 and
            loss['timeout_pressure']['running'] == 1 and loss['timeout_pressure']['uncertain_writes'] == 1 and
            loss['canonical'] == {'records': 1, 'relations': 0, 'receipts': 1, 'revision': 1,
                                  'kinds': {'Interaction': 1}}, 'Response-loss/replay invariant failed')
    phases = payload['phases']
    if [phase['concurrency'] for phase in phases] != profile['phases']['caller_concurrency']:
        raise ValueError('Phase concurrency does not match selected profile')
    summaries = []
    total = 0
    for phase in phases:
        operations = phase['operations']
        expected = phase['concurrency'] * profile['phases']['operations_per_caller']
        if len(operations) != expected:
            raise ValueError('Missing operation measurements')
        total += len(operations)
        metrics = {}
        for kind, percent in [('search', 60), ('read', 30), ('create', 10)]:
            samples = [item for item in operations if item['kind'] == kind]
            if len(samples) != expected * percent // 100:
                raise ValueError('Operation mix differs from selected profile')
            latencies = sorted(item['latency_us'] for item in samples)
            if not all(isinstance(value, int) and value >= 0 for value in latencies):
                raise ValueError('Invalid latency sample')
            outcomes = {name: sum(item['outcome'] == name for item in samples)
                        for name in ['success', 'conflict', 'capacity', 'storage', 'other']}
            if sum(outcomes.values()) != len(samples):
                raise ValueError('Unknown outcome')
            def percentile(percent):
                return latencies[(len(latencies) * percent + 99) // 100 - 1]
            metrics[kind] = {'attempted': len(samples), 'outcomes': outcomes,
                            'latency_us': {name: percentile(percent) for name, percent in
                                [('p50', 50), ('p95', 95), ('p99', 99)]} | {'max': latencies[-1]}}
        wall = phase['wall_us'] / 1_000_000
        derived = {kind: {name: count for name, count in metric['outcomes'].items() if count}
                   for kind, metric in metrics.items()}
        require(phase['counts'] == derived, 'Emitted counts differ from raw observations')
        canonical = phase['canonical']
        committed = canonical['receipts'] - baseline['receipts']
        created = metrics['create']['outcomes']
        require(created['success'] <= committed <= created['success'] + created['storage'],
                'Committed writes differ from observed outcomes')
        expected_canonical = dict(baseline, records=baseline['records'] + committed,
                                  receipts=baseline['receipts'] + committed, revision=baseline['revision'] + committed,
                                  kinds=dict(baseline['kinds'], Interaction=baseline['kinds']['Interaction'] + committed))
        require(canonical == expected_canonical, 'Canonical phase counters differ')
        require(all(metric['outcomes']['other'] == 0 for metric in metrics.values()), 'Unexpected operation error')
        require(phase['max_request_bytes'] <= profile['worker_limits']['max_submitted_request_bytes'] and
                phase['submitted_bytes'] > 0 and phase.get('submission_calls', phase.get('store_calls', 0)) >= expected,
                'Invalid submitted-byte measurements')
        if profile['profile_id'] == 'BB-PORTAL-DIAGNOSTIC-P2':
            require(phase['submission_unit'] == 'CRM commands' and phase['submission_calls'] == expected,
                    'P2 must submit exactly one whole command per attempt')
            require(all(metric['outcomes']['success'] == metric['attempted'] for metric in metrics.values()),
                    'P2 participating commands did not all complete successfully')
        maximum = phase['sampled_pressure_max']
        require(maximum['running'] <= 1 and maximum['pending'] <= profile['worker_limits']['pending'] and
                maximum['charged_bytes'] <= profile['worker_limits']['outstanding_bytes'], 'Worker pressure exceeds bounds')
        baseline = canonical
        if wall <= 0:
            raise ValueError('Nonpositive phase duration')
        summaries.append({'concurrency': phase['concurrency'], 'operations': expected,
                          'wall_seconds': wall, 'operations_per_second': expected / wall,
                          'metrics': metrics})
    if total != profile['phases']['operations_total']:
        raise ValueError('Total operation count differs from selected profile')
    require(payload['total_operations'] == total and payload['final_snapshot'] == baseline,
            'Final canonical state or emitted total differs')
    if profile['profile_id'] == 'BB-PORTAL-DIAGNOSTIC-P2':
        raw = payload['raw_canonical']
        require(payload['canonical_read_scope'] == 'direct shared raw MemoryStore after command drain',
                'P2 canonical read must bypass command worker')
        require(len(raw['records']) == baseline['records'] and len(raw['receipts']) == baseline['receipts'] and
                len(raw['relations']) == baseline['relations'] and int(raw['revision']) == baseline['revision'],
                'Raw canonical snapshot counters differ')
        require(raw['schema_version'] == 2 and not raw['fields'] and not raw['pricing_tiers'],
                'Unexpected canonical schema/catalogue changes')
        classifications = [record['client_terms']['classification'] for record in raw['records'].values()
                           if record['kind'] == 'Customer']
        require(classifications.count('B2C') == 20 and classifications.count('B2B') == 20,
                'Seed client classification differs')
        created_ids, revisions = set(), set()
        for phase_index, phase in enumerate(phases):
            for item in phase['operations']:
                if item['kind'] != 'create':
                    continue
                receipt = raw['receipts'][item['operation_id']]
                request = receipt['request']
                record = request['command']['Create']
                stored = raw['records'][record['id']]
                expected_record = dict(record, revision=1)
                require(receipt['actor'] == 'diagnostic' and request['operation_id'] == item['operation_id'] and
                        record['kind'] == 'Interaction' and int(record['revision']) == 0 and
                        stored == expected_record and
                        int(receipt['outcome']['record_revision']) == 1 and
                        160 < int(receipt['outcome']['workspace_revision']) <= baseline['revision'],
                        'Raw create receipt identity/outcome differs')
                require(item['operation_id'] not in created_ids, 'Repeated creation measurement identity')
                created_ids.add(item['operation_id'])
                revisions.add(int(receipt['outcome']['workspace_revision']))
        require(len(created_ids) == 210 and revisions == set(range(161, 371)),
                'Create receipts do not cover exactly one commit per submitted command')
    return summaries


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--executable', required=True)
    parser.add_argument('--profile', choices=['P1', 'P2'], default='P1')
    args = parser.parse_args()
    if os.name != 'nt':
        raise RuntimeError('P1 monitor currently proves Windows only')
    binary = Path(args.executable).resolve(strict=True)
    profile_key = 'command_provisional_profile' if args.profile == 'P2' else 'provisional_profile'
    profile = yaml.safe_load(SPEC.read_text(encoding='utf-8'))['research_basis']['swarm_admission'][profile_key]
    architecture = binary_architecture(binary)
    output = ROOT / '.local/portal-workload/runs' / uuid.uuid4().hex
    output.mkdir(parents=True)
    before = fingerprints(binary)
    result = {'status': 'failed', 'profile_id': profile['profile_id'], 'profile': profile,
              'architecture': architecture, 'host_architecture': platform.machine(),
              'source_hashes_before': before, 'artifact_directory': str(output)}
    child = None
    start = time.monotonic()
    try:
        with (output / 'stderr.txt').open('wb') as errors:
            child = subprocess.Popen([str(binary)] + (['--command'] if args.profile == 'P2' else []), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=errors, creationflags=subprocess.CREATE_NO_WINDOW)
            result['owned_pid'] = child.pid
            resources = OwnedResources(child)
            lines = queue.Queue(maxsize=1)
            def read_result():
                try:
                    lines.put(child.stdout.readline(1_048_577))
                except Exception as error:
                    lines.put(error)
            reader = threading.Thread(target=read_result, daemon=True)
            reader.start()
            count = 0
            while lines.empty():
                observation = resources.read()
                count += 1
                if observation['peak_working_set_bytes'] > profile['diagnostic_bounds']['observed_process_peak_working_set_ceiling_bytes']:
                    raise RuntimeError('Observed process working-set ceiling exceeded')
                if time.monotonic() - start >= profile['diagnostic_bounds']['run_limit_seconds']:
                    raise RuntimeError('Diagnostic run deadline exceeded')
                time.sleep(0.01)
            line = lines.get_nowait()
            if isinstance(line, Exception):
                raise line
            if not line.endswith(b'\n') or len(line) > 1_048_576:
                raise ValueError('Runner result missing or exceeds 1MiB')
            (output / 'runner.json').write_bytes(line)
            payload = json.loads(line)
            result['resources'] = resources.read()
            result['measurement_wall_seconds'] = time.monotonic() - start
            if result['measurement_wall_seconds'] >= profile['diagnostic_bounds']['run_limit_seconds']:
                raise RuntimeError('Final diagnostic measurement deadline exceeded')
            result['resource_samples'] = count + 1
            result['resource_scope'] = 'Process lifetime through result handshake; includes seeding/fault phases. CPU excludes Python monitor.'
            result['summaries'] = reconcile(payload, profile)
            if result['resources']['peak_working_set_bytes'] > profile['diagnostic_bounds']['observed_process_peak_working_set_ceiling_bytes']:
                raise RuntimeError('Final working-set ceiling exceeded')
            child.stdin.write(b'release\n')
            child.stdin.flush()
            child.stdin.close()
            result['exit_code'] = child.wait(timeout=5)
            reader.join(timeout=1)
            if reader.is_alive() or result['exit_code'] != 0:
                raise RuntimeError('Runner cleanup or exit failed')
            after = fingerprints(binary)
            result['source_hashes_after'] = after
            if before != after:
                raise RuntimeError('Measurement inputs changed during run')
            result['status'] = 'measured_provisional'
    except Exception as error:
        result['error'] = repr(error)
        raise
    finally:
        if child is not None:
            try:
                if child.poll() is None:
                    child.kill()  # Exact process started by this monitor, never a PID lookup.
                child.wait(timeout=5)
                result['owned_cleanup'] = 'exit_verified'
                result['exit_code'] = child.returncode
            except Exception as error:
                result['status'] = 'failed'
                result['owned_cleanup'] = 'failed'
                result['cleanup_error'] = repr(error)
        result['monitor_wall_seconds'] = time.monotonic() - start
        (output / 'result.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
        (ROOT / '.local/portal-workload/result.json').write_text(json.dumps(result, indent=2), encoding='utf-8')
    print(json.dumps({key: result[key] for key in ['status', 'architecture', 'resources', 'summaries', 'artifact_directory']}))


if __name__ == '__main__':
    main()
