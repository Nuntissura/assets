# Vendored HSRepoTemplate validate_spec.py (bootstrap 2.3.3); added extracted-YAML template loading.
"""Validate the declared core spec format, references, hashes and roadmap metadata.
This is source validation; it does not prove prose agreement, product readiness or runtime acceptance.
"""
from pathlib import Path
import argparse, datetime, hashlib, json, re, sys
import yaml
class SpecError(ValueError): pass
class UniqueLoader(yaml.SafeLoader): pass
def unique_mapping(loader,node,deep=False):
    result={}
    for k,v in node.value:
        key=loader.construct_object(k,deep=deep)
        if key in result: raise SpecError('duplicate YAML key: '+str(key))
        result[key]=loader.construct_object(v,deep=deep)
    return result
UniqueLoader.add_constructor(yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,unique_mapping)
def load_yaml(text): return yaml.load(text,Loader=UniqueLoader)
def unique_json(pairs):
    result={}
    for key,value in pairs:
        require(key not in result,'duplicate JSON key: '+key);result[key]=value
    return result
def require(test,message):
    if not test: raise SpecError(message)
def obj(value,where):
    require(isinstance(value,dict),where+' must be an object');return value
def arr(value,where):
    require(isinstance(value,list),where+' must be an array');return value
def text(value,where):
    require(isinstance(value,str) and value.strip(),where+' must be a nonempty string');return value
def exact_keys(record,required,optional,where):
    obj(record,where);require(set(required)<=set(record),where+' missing fields: '+str(sorted(set(required)-set(record))))
    require(set(record)<=set(required)|set(optional),where+' unknown fields: '+str(sorted(set(record)-set(required)-set(optional))))
def fence(source,pos,where):
    while pos<len(source) and source[pos].isspace():pos+=1
    m=re.match(r'(`{3,}|~{3,})([^\r\n]*)\r?\n',source[pos:])
    require(m is not None,where+' has unfenced content at character '+str(pos))
    marker=m.group(1);language=m.group(2).strip();start=pos+m.end()
    close=re.compile(r'^'+re.escape(marker[0])+'{'+str(len(marker))+r',}[ \t]*\r?$',re.M).search(source,start)
    require(close is not None,where+' has an unclosed fence')
    return language,source[start:close.start()],close.end()
def load_template(path):
    if path.suffix in ('.yaml', '.yml'):
        return obj(load_yaml(path.read_text(encoding='utf-8-sig')), 'template')
    source=path.read_text(encoding='utf-8-sig');m=re.search(r'<!-- handshake-template:begin[^>]*-->\s*',source)
    require(m is not None,'template marker missing');lang,body,_=fence(source,m.end(),'template')
    require(lang=='yaml','template payload must be YAML');return obj(load_yaml(body),'template')
def at_path(obj,path):
    for part in path.split('.'):obj=obj[part]
    return obj

def validate(root,template):
    root=root.resolve();manifest_path=root/'specification.yaml';require(manifest_path.is_file(),'specification.yaml missing')
    manifest=obj(load_yaml(manifest_path.read_text(encoding='utf-8-sig')),'manifest')
    require(manifest.get('schema_version')==template['schema_version'],'manifest schema version differs from selected template')
    identity=obj(manifest.get('identity'),'identity');specid=text(identity.get('specification_id'),'specification_id')
    storage=obj(manifest.get('storage'),'storage');documents=arr(storage.get('documents'),'storage.documents');require(documents,'empty document set')
    require(storage.get('external_content_dependencies_allowed',False) is False,'external content dependencies forbidden')
    require(storage.get('sidecars_required',False) is False,'required sidecars forbidden')
    policy=template['document_schema'];format_contract=template['topic_format_contract'];road_contract=template['roadmap_contract'];content_contract=template['document_content_contract']
    seen_docs=set();seen_paths=set();seen_topics=set();decl={};records=[];road=[];block_count=0;document_metadata=[]
    identity_keys={'anchors':'anchor_id','requirements':'requirement_id','acceptance':'acceptance_id','features':'feature_id','primitives':'primitive_id','decisions':'decision_id','primitive_feature_relations':'relation_id','gui_design':'design_id'}
    project_fields={}
    for d in documents:
        exact_keys(d,storage.get('document_record_required_fields',template['storage']['document_record_required_fields']),[], 'document record')
        rel=text(d['path'],'document path');relative=Path(rel)
        require(not relative.is_absolute() and not relative.drive and '..' not in relative.parts,'document path escapes spec root: '+rel)
        p=(root/relative).resolve();require(p.is_relative_to(root),'document path escapes through link: '+rel)
        require(p.is_file(),'document missing: '+rel);require(rel not in seen_paths,'duplicate document path: '+rel);seen_paths.add(rel)
        raw=p.read_bytes();digest='sha256:'+hashlib.sha256(raw).hexdigest();require(d['digest']==digest,'document digest mismatch: '+rel)
        source=raw.decode('utf-8-sig');fm=re.match(r'^---\r?\n(.*?)\r?\n---(?:\r?\n|$)',source,re.S);require(fm is not None,'frontmatter missing: '+rel)
        metadata=obj(load_yaml(fm.group(1)),'frontmatter '+rel)
        exact_keys(metadata,policy['frontmatter_required'],policy.get('frontmatter_optional',{}),'frontmatter '+rel)
        require(metadata['schema_version']==policy['schema_version'],'document schema differs: '+rel)
        require(metadata['specification_id']==specid,'wrong specification id: '+rel)
        docid=text(metadata['document_id'],'document id');require(docid==d['document_id'],'manifest/document id mismatch: '+rel);require(docid not in seen_docs,'duplicate document id: '+docid);seen_docs.add(docid)
        require(type(metadata['revision']) is int and metadata['revision']>0,'invalid document revision: '+rel);require(metadata['revision']==d['revision'],'revision mismatch: '+rel);text(metadata['title'],'document title')
        document_metadata.append(metadata)
        pos=fm.end();local_ids=set();topic_count=0
        while source[pos:].strip():
            lang,header_text,pos=fence(source,pos,rel)
            require(lang=='yaml','topic header must be YAML: '+rel)
            h=obj(load_yaml(header_text),'topic header '+rel)
            exact_keys(h,format_contract['header_required_fields'],[],'topic header '+rel)
            require(h['kind']=='specification_topic','invalid header kind: '+rel)
            tid=text(h['topic_id'],'topic id');require(tid not in seen_topics and tid not in decl,'duplicate topic id: '+tid);seen_topics.add(tid);local_ids.add(tid);topic_count+=1
            text(h['title'],'topic title');blocks=arr(h['content_blocks'],'content_blocks');require(blocks,'empty topic blocks: '+tid);block_ids=set()
            for desc in blocks:
                exact_keys(desc,format_contract['content_block_descriptor_fields'],[],'block descriptor '+tid)
                bid=text(desc['block_id'],'block id');require(bid not in block_ids,'duplicate block id: '+tid+'/'+bid);block_ids.add(bid)
                require(desc['kind'] in format_contract['content_block_kinds'],'unknown content block kind: '+tid)
                arr(desc['requirement_refs'],'descriptor requirement_refs');records.append(('descriptor',desc,tid))
                lang,body,pos=fence(source,pos,tid+'/'+bid);block_count+=1
                require(lang==desc['language'],'content language does not match header: '+tid+'/'+bid)
                if desc['kind']=='prose':require(lang=='markdown','prose must use markdown fence: '+tid)
                elif desc['kind']=='structured_contract':
                    require(lang in ('yaml','json'),'structured contract must be YAML/JSON: '+tid)
                    content=obj(load_yaml(body) if lang=='yaml' else json.loads(body,object_pairs_hook=unique_json),'structured content '+tid)
                    require(set(content)<=set(policy['structured_record_collections']),'unknown structured collection: '+str(set(content)-set(policy['structured_record_collections'])))
                    for collection,values in content.items():
                        for record in arr(values,collection):
                            obj(record,collection+' record');records.append((collection,record,tid))
                            if collection=='project_field_definitions':
                                exact_keys(record,policy['project_field_definition_fields'],[],'project field definition')
                                project_fields[(record['scope'],record['field_name'])]=record
                            if collection in identity_keys:
                                key=identity_keys[collection]
                                if key not in record and collection=='requirements':continue
                                entity=text(record.get(key),key);require(entity not in decl and entity not in seen_topics,'duplicate semantic id: '+entity);decl[entity]=collection;local_ids.add(entity)
                            elif collection=='roadmap':road.append(record)
            require(source[pos:].strip()=='' or source[pos:].lstrip().startswith(('```yaml','~~~yaml')),'content outside topic framing: '+rel)
        require(topic_count>0,'no topic headers: '+rel)
        require(set(arr(d['semantic_entity_refs'],'semantic_entity_refs'))==local_ids,'manifest semantic declaration inventory mismatch: '+rel)
    declared_files={p.relative_to(root).as_posix() for p in (root/storage.get('document_root','spec-modules/')).rglob('*.md')}
    require(declared_files==seen_paths,'unlisted or missing spec documents: '+str(declared_files^seen_paths))
    for collection,r,tid in records:
        if collection=='descriptor':continue
        fields_ref=policy['structured_record_collections'][collection]
        required=at_path(template,fields_ref) if '.' in fields_ref else content_contract[fields_ref]
        optional=content_contract.get('requirement_optional_fields',[]) if collection=='requirements' else []
        if collection=='requirements' and 'requirement_id' not in r:required=[x for x in required if x!='requirement_id']
        if collection=='roadmap' and r.get('implementation_status')=='retired':required=list(required)+road_contract['retirement']['required_inline_context']
        optional=list(optional)+[k for (scope,k) in project_fields if scope==collection]
        exact_keys(r,required,optional,collection+' in '+tid)
        for field in ('statement','rationale','decision','title','method','expected_result','contract'):
            if field in r:text(r[field],collection+'.'+field)
        for field in ('conditions','exceptions'):
            if field in r:
                for clause in arr(r[field],field):text(clause,field+' clause')
    semantic_kinds=set(identity_keys)|{'topic'}
    targets={'requirement_refs':{'requirements'},'acceptance_refs':{'acceptance'},'feature_refs':{'features'},'primitive_refs':{'primitives'},'primitive_ref':{'primitives'},'feature_ref':{'features'},'anchor_refs':{'anchors','topic'},'source_topic_refs':{'topic'},'constraint_refs':{'requirements','anchors','decisions'},'interaction_contract_refs':{'topic','primitives','requirements'},'item_ref':semantic_kinds,
             'visual_design_refs':semantic_kinds,'theme_and_component_refs':semantic_kinds,'surface_profile_refs':semantic_kinds,'replacement_refs':semantic_kinds,'supersedes':semantic_kinds}
    # These are optional provenance or downstream links; their destinations need not
    # live in the spec. Required GUI/product meaning still resolves through targets.
    external_reference_fields={'source_refs','research_source_refs','topology_refs','matrix_entry_refs'}
    for metadata in document_metadata:
        for ref in arr(metadata.get('supersedes',[]),'document supersedes'):
            text(ref,'document supersedes');require(ref in seen_docs,'unresolved document supersedes: '+ref)
            require(ref!=metadata['document_id'],'document cannot supersede itself: '+ref)
        for ref in arr(metadata.get('provenance',[]),'document provenance'):text(ref,'document provenance')
    for collection,r,tid in records:
        for key in external_reference_fields & set(r):
            for ref in arr(r[key],key):text(ref,key)
        for key,kinds in targets.items():
            if key not in r:continue
            values=[r[key]] if key.endswith('_ref') else arr(r[key],key)
            for ref in values:
                text(ref,key);kind='topic' if ref in seen_topics else decl.get(ref);require(kind in kinds,'unresolved or mistyped '+key+': '+ref+' in '+tid)
                if key=='supersedes':require(ref!=r.get(identity_keys.get(collection)), 'declaration cannot supersede itself: '+ref)
    req_records={r['requirement_id']:r for c,r,_ in records if c=='requirements' and 'requirement_id' in r}
    feature_records={r['feature_id']:r for c,r,_ in records if c=='features'}
    feature_ids=set(feature_records);road_ids=set();covered=set()
    for r in road:
        item=r['item_ref'];require(item not in road_ids,'duplicate roadmap item: '+item);road_ids.add(item)
        require(r['implementation_status'] in road_contract['implementation_statuses'],'invalid roadmap status: '+item)
        implemented=set(r['implemented_requirement_refs']);remaining=set(r['remaining_requirement_refs'])
        require(not implemented&remaining,'overlapping roadmap scopes: '+item)
        require((implemented|remaining)<=set(req_records),'unknown roadmap requirement: '+item);covered|=implemented|remaining
        if item in feature_records:require((implemented|remaining)==set(feature_records[item]['requirement_refs']),'roadmap feature scope mismatch: '+item)
        require(r['verification_summary'].strip(),'empty verification summary: '+item)
        if r['implementation_status']=='planned':require(not implemented,'planned row claims implemented requirements: '+item)
        if r['implementation_status']=='implemented':require(not remaining and r['implementation_source_revision'] is not None and bool(r['matrix_entry_refs']),'implemented row missing completion metadata: '+item)
    require(feature_ids<=road_ids,'features missing from roadmap: '+str(feature_ids-road_ids))
    require(set(req_records)<=covered,'requirements missing from roadmap coverage: '+str(set(req_records)-covered))
    for rid,r in req_records.items():
        require(r['acceptance_refs'],'requirement has no acceptance: '+rid)
        for aid in r['acceptance_refs']:
            a=next(v for c,v,_ in records if c=='acceptance' and v['acceptance_id']==aid)
            require(rid in a['requirement_refs'],'requirement/acceptance backlink mismatch: '+rid)
    return {'status':'passed','scope':'core_source_structure_and_internal_references_only','documents':len(documents),'topics':len(seen_topics),'content_blocks':block_count,'requirements':len(req_records),'features':len(feature_ids),'primitives':sum(x=='primitives' for x in decl.values()),'roadmap_items':len(road),'checks':['duplicate_keys','declared_fences_and_block_counts','core_record_fields','unique_semantic_ids','typed_internal_references','manifest_hashes_revisions_membership','roadmap_status_and_coverage','acceptance_backlinks'],'not_proven':['natural_language_semantic_agreement','complete_operator_intent_preservation','runtime_or_product_acceptance','template_release_readiness']}
def main():
    p=argparse.ArgumentParser();p.add_argument('spec_root',type=Path);p.add_argument('--template',required=True,type=Path);p.add_argument('--json',action='store_true');args=p.parse_args()
    try:result=validate(args.spec_root,load_template(args.template))
    except (SpecError,yaml.YAMLError,ValueError,KeyError,TypeError,OSError) as e:result={'status':'failed','error':str(e)}
    print(json.dumps(result,indent=2,default=str));return 0 if result['status']=='passed' else 1
if __name__=='__main__':sys.exit(main())
