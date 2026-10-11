"""Rebuild small navigation projections; --check compares without writing."""
import argparse
import collections
import hashlib
import json
import os
import sys
from pathlib import Path

import yaml

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]
GOV = ROOT / 'repo-governance'

def encoded(value):
    return (yaml.safe_dump(value, sort_keys=False, allow_unicode=True, width=110)).encode()

def json_bytes(value):
    return (json.dumps(value, ensure_ascii=False, indent=2) + '\n').encode()

def load(path):
    return yaml.safe_load(path.read_text(encoding='utf-8'))

def spec_records():
    from spec_check import validate, load_template
    current = GOV / 'specification/current'
    template = GOV / 'templates/specification/TECHNICAL-SPECIFICATION.template.yaml'
    result = validate(current, load_template(template))
    manifest = load(current / 'specification.yaml')
    records = collections.defaultdict(list)
    for doc in manifest['storage']['documents']:
        text = (current / doc['path']).read_text(encoding='utf-8')
        import re
        for block in re.findall(r'^```yaml\n(.*?)\n```', text, re.M | re.S):
            value = yaml.safe_load(block)
            if value.get('kind') == 'specification_topic':
                continue
            for collection, rows in value.items():
                records[collection].extend(rows)
    return result, manifest, records

def projections():
    source = load(GOV / 'codex/current/codex-source.yaml')
    code = '# Kumihimo model instructions\n\nGenerated from `current/codex-source.yaml`; edit the source and run `python repo-governance/repo-scripts/sync.py`.\n\n'
    code += 'Read the taskboard, selected WP contract/state, relevant spec topics and affected build rules. The selected minimal profile controls execution; reference templates introduce no extra gates.\n\n'
    code += 'Build rules: `build-rules/current/build-rules.yaml`. Workflow: `workflows/repository-workflows.md`.\n\n'
    code += '\n\n'.join('[' + row['rule_id'] + '] ' + row['requirement'] for row in source['rules']) + '\n'
    files = {'repo-governance/codex/CODEX.md': code.encode()}
    rows = []
    for contract_path in sorted((GOV / 'work-packets/active').glob('*/WP-*-contract.yaml')):
        contract = load(contract_path)
        state = load(contract_path.parent / (contract['wp_id'] + '-state.yaml'))
        if state['wp_id'] != contract['wp_id'] or state['phase'] not in {'planned','active','blocked','done','cancelled'}:
            raise ValueError('Invalid work packet state: ' + contract['wp_id'])
        rows.append({'wp_id': contract['wp_id'], 'title': contract['title'], 'phase': state['phase'],
                     'contract_ref': contract_path.relative_to(ROOT).as_posix(),
                     'state_ref': (contract_path.parent / (contract['wp_id'] + '-state.yaml')).relative_to(ROOT).as_posix(),
                     'next_action': state['next_action'], 'blockers': state['blockers']})
    files['repo-governance/taskboard/taskboard.yaml'] = encoded({'schema_version':'kumihimo.taskboard.v1', 'kind':'derived_taskboard', 'project_id':'KUMIHIMO', 'source':'WP contracts and state', 'counts':dict(collections.Counter(row['phase'] for row in rows)), 'work_packets':rows})
    files['repo-governance/registry/work-packets.yaml'] = encoded({'kind':'derived_work_packet_registry','project_id':'KUMIHIMO','work_packets':rows})
    result, manifest, records = spec_records()
    current = 'repo-governance/specification/current/'
    files[current + 'INDEX.json'] = json_bytes({'kind':'derived_spec_index','specification_id':manifest['identity']['specification_id'],'revision':manifest['identity']['revision'],'documents':manifest['storage']['documents']})
    files[current + 'primitive-list.json'] = json_bytes({'kind':'derived_primitive_list','spec_revision':manifest['identity']['revision'],'primitives':records['primitives']})
    files[current + 'primitive-feature-mix-matrix.json'] = json_bytes({'kind':'derived_primitive_feature_matrix','spec_revision':manifest['identity']['revision'],'relations':records['primitive_feature_relations']})
    ids = [row['rule_id'] for row in source['rules']] + [row['wp_id'] for row in rows]
    for doc in manifest['storage']['documents']:
        ids += [doc['document_id']] + doc['semantic_entity_refs']
    if len(ids) != len(set(ids)):
        raise ValueError('Duplicate registry identity')
    files['repo-governance/registry/ids.yaml'] = encoded({'kind':'derived_id_registry','project_id':'KUMIHIMO','ids':ids})
    return files, result

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    files, result = projections()
    stale = []
    for rel, data in files.items():
        path = ROOT / rel
        if path.is_file() and path.read_bytes() == data:
            continue
        stale.append(rel)
        if not args.check:
            path.parent.mkdir(parents=True, exist_ok=True)
            temporary = path.with_name(path.name + '.sync-tmp')
            temporary.write_bytes(data)
            os.replace(temporary, path)
    print(json.dumps({'status':'failed' if args.check and stale else 'passed', 'mode':'check' if args.check else 'sync', 'changed_or_stale':stale, 'spec_structure':result}, indent=2))
    return 1 if args.check and stale else 0

if __name__ == '__main__':
    raise SystemExit(main())
