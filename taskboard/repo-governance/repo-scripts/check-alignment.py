"""Read-only owner-template checks; no build, activation or product-behavior verdict."""
from pathlib import Path
import argparse
import hashlib
import importlib.util
import json
import os
import re
import subprocess
import sys
import yaml

ROOT = Path(__file__).resolve().parents[2]

def read(path):
    return yaml.safe_load((ROOT / path).read_text(encoding='utf-8-sig'))

def require(condition, message):
    if not condition:
        raise ValueError(message)

def resolve(ref):
    path, _, pointer = ref.partition('#')
    target = (ROOT / path).resolve()
    require(target.is_relative_to(ROOT), 'Reference escapes module: ' + ref)
    require(target.exists(), 'Missing reference: ' + ref)
    if pointer:
        value = yaml.safe_load(target.read_text(encoding='utf-8-sig'))
        require(pointer.startswith('/'), 'Invalid pointer: ' + ref)
        for part in pointer[1:].split('/'):
            part = part.replace('~1', '/').replace('~0', '~')
            value = value[int(part)] if isinstance(value, list) else value[part]
    return target

def run(args):
    process = subprocess.run(args, cwd=ROOT, capture_output=True, text=True)
    require(process.returncode == 0, process.stdout + process.stderr)
    return json.loads(process.stdout)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--skill-root', type=Path)
    parser.add_argument('--preservation', action='store_true', help='Check initial adoption snapshot only')
    args = parser.parse_args()
    configured = os.environ.get('HSRT_SKILL_ROOT')
    codex_root = Path(os.environ.get('CODEX_HOME', Path.home() / '.codex'))
    skill = args.skill_root or (Path(configured) if configured else codex_root / 'skills/hsrepotemplate')
    require((skill / 'scripts/validate_spec.py').is_file(), 'Set --skill-root or HSRT_SKILL_ROOT to installed hsrepotemplate')
    adoption = read('repo-governance/hydration/alignment.yaml')
    require(adoption['scope']['sibling_projects_in_scope'] is False, 'Sibling scope changed')
    require(adoption['layout']['relocations'] == [], 'Alignment unexpectedly relocates product')
    require((ROOT / 'AGENTS.md').read_bytes() == (ROOT / 'CLAUDE.md').read_bytes(), 'Authority mirror drift')
    require(not (ROOT / '.git').exists(), 'Unexpected nested Git repository')
    for update in adoption.get('updates', []):
        archive = resolve(update['archive_ref'])
        hashes = {path.relative_to(archive).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in archive.rglob('*') if path.is_file()}
        require(hashes == update['archive_file_hashes'], 'Archived specification changed: ' + update['event_id'])
        digest = hashlib.sha256((json.dumps(hashes, indent=2, sort_keys=True, ensure_ascii=False) + '\n').encode()).hexdigest()
        require(digest == update['archive_bundle_sha256'], 'Archived specification bundle digest mismatch')
    for name, digest in adoption['preserved_input_hashes'].items():
        require((ROOT / name).is_file(), 'Preserved package file missing: ' + name)
        if args.preservation:
            require('sha256:' + hashlib.sha256((ROOT / name).read_bytes()).hexdigest() == digest,
                    'Original alignment bytes changed: ' + name)
    pin_rows = adoption['template_inputs']['members']
    manifest = read('repo-governance/templates/TEMPLATE-PACK.template.yaml')
    require(hashlib.sha256((ROOT / 'repo-governance/templates/TEMPLATE-PACK.template.yaml').read_bytes()).hexdigest() == adoption['template_inputs']['payload_sha256'], 'Template manifest digest drift')
    require(manifest['identity']['version'] == adoption['template_inputs']['version'], 'Template pack version drift')
    for member in manifest['templates']:
        pin = next(p for p in pin_rows if p['template_id'] == member['template_id'])
        source = ROOT / 'repo-governance/templates' / member['path'].replace('.md', '.yaml')
        require(source.is_file(), 'Template missing: ' + str(source))
        require(hashlib.sha256(source.read_bytes()).hexdigest() == pin['payload_sha256'], 'Template seed drift: ' + member['path'])
        require(member['template_version'] == pin['template_version'], 'Template version drift: ' + member['path'])
    # The validator implementation is installed; its contract input is pinned here.
    # Equivalent to its CLI --template input, without a duplicate Markdown seed.
    imported = importlib.util.spec_from_file_location('hsrt_spec_validator', skill / 'scripts/validate_spec.py')
    validator = importlib.util.module_from_spec(imported)
    imported.loader.exec_module(validator)
    spec = validator.validate(ROOT / 'repo-governance/specification/current',
                              read('repo-governance/templates/specification/TECHNICAL-SPECIFICATION.template.yaml'))
    require(spec['status'] == 'passed', 'Specification validation failed')
    spec_source = read('repo-governance/specification/current/specification.yaml')
    for actor in [spec_source['authority']['controller'], spec_source['derived_indexes']['policy']['controller'],
                  spec_source['update_protocol']['controller'], spec_source['roadmap_contract']['status_controller']]:
        require(actor == 'authorized_module_maintainer', 'Instantiated specification controller differs from selected module owner')
    schema_files = {'WP-CONTRACT':'wp-contract', 'MICROTASK':'microtask', 'WP-STATE':'wp-state',
                    'REFINEMENT':'refinement-enrichment', 'RECEIPT':'receipt', 'ROLE-PROFILE':'role-profile', 'GAMEPLAN':'gameplan'}
    for name, target in schema_files.items():
        require((ROOT / 'repo-governance/schemas' / (target + '.schema.yaml')).read_bytes() ==
                (ROOT / 'repo-governance/templates/schemas' / (name + '-SCHEMA.template.yaml')).read_bytes(), 'Local schema drift: ' + target)
    module_report = run([sys.executable, '-B', str(skill / 'scripts/validate_artifacts.py'),
                         '--schemas', str(ROOT / 'repo-governance/schemas'),
                         '--module-manifest', str(ROOT / 'module.yaml'), '--json'])
    require(module_report['status'] == 'PASS', 'Module build-unit shape failed')
    module = read('module.yaml')
    semantic_ids = {identity for doc in read('repo-governance/specification/current/specification.yaml')['storage']['documents']
                    for identity in doc['semantic_entity_refs']}
    for identity in module['semantic_code']['specification_entity_refs'] + module['semantic_code']['canonical_capability_refs']:
        require(identity in semantic_ids, 'Module semantic reference unresolved: ' + identity)
    owner = read('repo-governance/templates/repository/MODULE.template.yaml')
    contract = owner['validation']['maintenance_contract']
    require(module['schema_version'] in contract['required_schema_versions'], 'Module schema mismatch')
    maintenance = module['maintenance']
    require(maintenance['contract_version'] == contract['contract_version'], 'Maintenance version mismatch')
    require(maintenance['unit_kind'] in contract['unit_kinds'], 'Maintenance kind invalid')
    for section, fields in contract['required_fields'].items():
        record = maintenance if section == 'root' else maintenance[section]
        for field in fields:
            require(record.get(field) not in (None, '', []), 'Missing maintenance field: ' + section + '.' + field)
    for field in contract['nonempty_list_fields']:
        section, name = field.split('.')
        require(isinstance(maintenance[section][name], list) and bool(maintenance[section][name]), 'Empty maintenance list: ' + field)
    require(re.fullmatch(contract['canonical_repository_url_pattern'], maintenance['source']['canonical_repository_url']), 'Invalid canonical source URL')
    require(bool(maintenance['embedding']['adapter_contract_refs']) == bool(maintenance['embedding']['conformance_proof_refs']), 'Adapter/proof pair incomplete')
    def refs(value):
        if isinstance(value, dict):
            for entry in value.values(): refs(entry)
        elif isinstance(value, list):
            for entry in value: refs(entry)
        elif isinstance(value, str) and value.startswith('repo-governance/'):
            resolve(value)
    refs(maintenance)
    resolve(module['navigation']['topology_node_ref'])
    for path in module['boundaries']['public_surfaces']: resolve(path)
    registry = read('repo-governance/registry/ids.yaml')
    ids = [entry['id'] for entry in registry['entries']]
    require(len(ids) == len(set(ids)), 'Duplicate governance ID')
    for entry in registry['entries']: resolve(entry['path'])
    topology = read('repo-governance/topology/product.yaml')
    require(topology['nodes'][0]['module_id'] == module['identity']['module_id'], 'Topology/module identity drift')
    governance = read('repo-governance/topology/governance.yaml')
    node_ids = {row['node_id'] for row in governance['nodes']} | {row['node_id'] for row in topology['nodes']}
    for node in governance['nodes']: resolve(node['path'])
    for edge in governance['links']:
        require(edge['source'] in node_ids and edge['target'] in node_ids, 'Topology edge unresolved')
    work = read('repo-governance/registry/work-packets.yaml')
    board = read('repo-governance/taskboard/taskboard.yaml')
    for identity in [registry['registry_id'], topology['identity']['topology_id'], board['projection']['board_id'],
                     board['projection']['projection_id'], 'TASKBOARD-ENTRY-001', board['stock_view_shape']['rule_id']]:
        require(identity in ids, 'Durable governance ID not registered: ' + identity)
    require(board['projection']['source_set_generation'] == work['generation'], 'Board generation stale')
    require(board['projection']['source_set_digest'] == 'sha256:' + hashlib.sha256((ROOT / 'repo-governance/registry/work-packets.yaml').read_bytes()).hexdigest(), 'Board source digest stale')
    require(board['summary']['total_work_packets'] == len(work['entries']), 'Board count differs from canonical registry')
    require(not work['entries'] and not board['work_packets'], 'Planning registry now contains work; reassess projection workflow')
    print(json.dumps({'status':'PASS', 'checks':['specification','module_build_unit','maintenance_shape_and_refs','template_pins','authority_mirror','topology','id_registry','planning_board'] + (['original_bytes_preserved'] if args.preservation else []),
                      'specification_report':spec,'module_report':module_report,
                      'proves_product_behavior':False,'full_repository_bootstrap':False}, indent=2))

if __name__ == '__main__':
    try:
        main()
    except (ValueError, KeyError, OSError, StopIteration, yaml.YAMLError, json.JSONDecodeError) as exc:
        print(json.dumps({'status':'FAIL','error':str(exc)}))
        sys.exit(1)
