"""Check lightweight governance links/statuses, not product behavior or proof authenticity."""
from pathlib import Path
import argparse
import copy
import json
import tempfile
import yaml

ROOT = Path(__file__).resolve().parents[1]
STATUSES = {'proposed', 'planned', 'in_progress', 'blocked', 'completed', 'superseded', 'cancelled'}


class UniqueLoader(yaml.SafeLoader):
    pass


def unique(loader, node, deep=False):
    result = {}
    for key, value in node.value:
        key = loader.construct_object(key, deep=deep)
        if key in result:
            raise ValueError(f'duplicate YAML key: {key}')
        result[key] = loader.construct_object(value, deep=deep)
    return result


UniqueLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, unique)


def check(root):
    root = root.resolve()

    def load(ref):
        path = (root / ref).resolve()
        if Path(ref).is_absolute() or not path.is_relative_to(root):
            raise ValueError(f'path outside repo: {ref}')
        return yaml.load(path.read_text(encoding='utf-8'), Loader=UniqueLoader)

    def require(condition, message):
        if not condition:
            raise ValueError(message)

    spec = load('repo-governance/specification/current/specification.yaml')
    rows = load('repo-governance/taskboard/taskboard.yaml')['work_packets']
    checks = load('repo-governance/codex/build-rules/current/build-rules.yaml')['checks']
    requirements = {r['requirement_id']: r for r in spec['requirements']}
    require(len(requirements) == len(spec['requirements']), 'duplicate spec requirement ID')
    roadmap = {r['wp_id']: r for r in spec['roadmap']}
    require(len(roadmap) == len(spec['roadmap']), 'duplicate roadmap WP')
    ids = {r['wp_id'] for r in rows}
    require(len(ids) == len(rows), 'duplicate taskboard WP')
    require(ids == set(roadmap), 'taskboard/spec roadmap membership drift')
    known_checks = {c['check_id'] for c in checks}
    require(len(known_checks) == len(checks), 'duplicate check ID')
    contracts = set()
    mt_count = 0

    def state_checks(contract, state, entity_id, replacements):
        status = state['status']
        require(status in STATUSES, f'{entity_id}: invalid status')
        require(state.get('sync_pending') is False, f'{entity_id}: recovery synchronization pending')
        for key in ('work_location', 'next_action'):
            require(isinstance(state.get(key), str) and state[key].strip(), f'{entity_id}: missing {key}')
        for key in ('progress', 'blockers', 'verification'):
            require(isinstance(state.get(key), list), f'{entity_id}: missing {key} list')
        require(isinstance(state.get('recovery'), dict), f'{entity_id}: missing recovery')
        location = (root / state['work_location']).resolve()
        require(location.is_relative_to(root) and location.exists(), f'{entity_id}: invalid work location')
        require(status != 'blocked' or bool(state['blockers']), f'{entity_id}: blocked without blocker')
        required_checks = set(contract.get('verification_checks', []))
        require(required_checks <= known_checks, f'{entity_id}: unknown verification check')
        if status == 'completed':
            proof = state.get('acceptance_proof')
            require(isinstance(proof, list) and len(proof) == len(contract['acceptance'])
                    and all(isinstance(p, str) and p.strip() for p in proof), f'{entity_id}: missing acceptance proof')
            passed = {v['check_id'] for v in state['verification'] if v.get('result') == 'passed'
                      and v.get('command') and v.get('observed')}
            require(required_checks <= passed, f'{entity_id}: completion checks missing')
        if status in {'superseded', 'cancelled'}:
            require(bool(state.get('reason')), f'{entity_id}: missing retirement reason')
        if status == 'superseded':
            require(state.get('superseded_by') in replacements and state['superseded_by'] != entity_id,
                    f'{entity_id}: unresolved replacement')

    for row in rows:
        wid = row['wp_id']
        c, s = load(row['contract_ref']), load(row['state_ref'])
        require(c['wp_id'] == s['wp_id'] == wid, f'{wid}: identity drift')
        require(row['status'] == s['status'] == roadmap[wid]['status'], f'{wid}: status drift')
        require(c['kind'] == row['kind'] and c['kind'] in {'active', 'stub'}, f'{wid}: kind drift')
        require(c['kind'] != 'stub' or s['status'] in {'proposed', 'planned', 'superseded', 'cancelled'},
                f'{wid}: executable stub')
        require(0 < c['spec_revision'] <= spec['revision'], f'{wid}: invalid spec revision')
        require(c.get('requirement_refs') and set(c['requirement_refs']) <= requirements.keys(),
                f'{wid}: unresolved spec requirements')
        require(c['requirement_refs'] == roadmap[wid]['requirement_refs'], f'{wid}: scope drift')
        require(c.get('scope') and c.get('acceptance'), f'{wid}: empty scope/acceptance')
        require(set(c.get('dependencies', [])) <= ids - {wid}, f'{wid}: unresolved dependencies')
        if c['kind'] == 'active':
            require(all(requirements[x]['status'] == 'accepted' for x in c['requirement_refs']),
                    f'{wid}: proposed spec scope cannot execute')
        state_checks(c, s, wid, ids)
        contracts.add(row['contract_ref'])
        mts = c.get('microtasks', [])
        mt_ids = set()
        mt_records = [(mt, load(mt['contract_ref']), load(mt['state_ref'])) for mt in mts]
        replacement_ids = {mc['mt_id'] for _, mc, _ in mt_records}
        for mt, mc, ms in mt_records:
            mid = mc['mt_id']
            require(mid == ms['mt_id'] and mid not in mt_ids and mc['wp_id'] == wid, f'{wid}: MT identity drift')
            parent = (root / row['contract_ref']).parent
            require((root / mt['contract_ref']).resolve().is_relative_to(parent)
                    and (root / mt['state_ref']).resolve().is_relative_to(parent), f'{mid}: MT outside WP')
            require(set(mc['requirement_refs']) <= set(c['requirement_refs']), f'{mid}: MT outside WP scope')
            require(s.get('microtasks', {}).get(mid) == ms['status'], f'{mid}: parent/MT status drift')
            state_checks(mc, ms, mid, replacement_ids)
            require(s['status'] != 'completed' or ms['status'] in {'completed', 'superseded', 'cancelled'},
                    f'{wid}: unfinished MT')
            mt_ids.add(mid)
            mt_count += 1
        require(set(s.get('microtasks', {})) == mt_ids, f'{wid}: MT membership drift')
    actual = {p.relative_to(root).as_posix() for p in (root / 'repo-governance/work-packets').rglob('wp-contract.yaml')}
    require(actual == contracts, 'unlisted/missing WP contract')
    require((root / 'AGENTS.md').read_bytes() == (root / 'CLAUDE.md').read_bytes(), 'provider authority drift')
    return {'status': 'passed', 'scope': 'structural_consistency_only', 'work_packets': len(rows),
            'microtasks': mt_count, 'requirements': len(requirements)}


def self_test():
    import shutil
    cases = [
        ('status drift', lambda d: d['work_packets'][0].update(status='completed')),
        ('duplicate WP', lambda d: d['work_packets'].append(copy.deepcopy(d['work_packets'][0]))),
        ('escaping path', lambda d: d['work_packets'][0].update(state_ref='../outside.yaml')),
    ]
    with tempfile.TemporaryDirectory(prefix='buurtman-check-') as temp:
        root = Path(temp)
        shutil.copytree(ROOT / 'repo-governance', root / 'repo-governance')
        for name in ('AGENTS.md', 'CLAUDE.md'):
            shutil.copyfile(ROOT / name, root / name)
        path = root / 'repo-governance/taskboard/taskboard.yaml'
        original = path.read_text(encoding='utf-8')
        check(root)
        for name, mutate in cases:
            data = yaml.safe_load(original)
            mutate(data)
            path.write_text(yaml.safe_dump(data), encoding='utf-8')
            try:
                check(root)
            except (ValueError, FileNotFoundError):
                continue
            raise ValueError(f'self-test failed to reject {name}')
        path.write_text(original, encoding='utf-8')
    return {'self_test': 'passed', 'negative_cases': len(cases)}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    try:
        print(json.dumps(self_test() if args.self_test else check(ROOT)))
    except (ValueError, KeyError, TypeError, OSError, yaml.YAMLError) as exc:
        print(json.dumps({'status': 'failed', 'error': str(exc)}))
        raise SystemExit(1)
