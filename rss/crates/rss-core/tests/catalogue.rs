use rss_core::{SourceGroup, SourceLibrary, ProfileSelection, MemberRule, RuleLimits, group_items, update_state};
use rss_contracts::*;
use std::collections::BTreeSet;
fn r(id:&str)->HostRef { HostRef { host_instance_id:"host".into(), owner_kind:"rss".into(), native_id:id.into() } }
fn bounds()->Bounds { Bounds { max_url_bytes:4096, max_text_bytes:4096, max_collection_items:100, max_page_items:25, max_operation_bytes:100000 } }
fn group(id:&str, scope:&ScopeRef)->SourceGroup { SourceGroup { group_ref:r(id), owner_scope:scope.clone(), name:id.into(), description:"".into(), keywords:vec![], priority:0, tag_refs:BTreeSet::new(), revision:Revision(1), common:Extensions::default() } }
fn library()->SourceLibrary {
    let scope=ScopeRef("account".into()); let library=SourceLibrary::empty(scope.clone());
    let source=Source { source_ref:r("source"), owner_scope:scope.clone(), provider:Provider::Instagram,
        original_url:"https://www.instagram.com/example/".into(), display_name:"Example".into(), revision:Revision(1), paused:false, common:Extensions::default() };
    let library=library.add_source(&scope,library.revision,source,&bounds()).unwrap();
    let library=library.save_group(&scope,library.revision,group("one",&scope),&bounds()).unwrap();
    library.save_group(&scope,library.revision,group("two",&scope),&bounds()).unwrap()
}
#[test]
fn multiple_groups_reuse_one_source_and_removal_preserves_it() {
    let library=library(); let scope=library.owner_scope.clone();
    assert_eq!(library.ungrouped_sources(&BTreeSet::from([r("source")])).len(),1);
    let library=library.attach(&scope,library.revision,&r("one"),&r("source"),&bounds()).unwrap();
    let same=library.attach(&scope,library.revision,&r("one"),&r("source"),&bounds()).unwrap();
    assert_eq!(same,library);
    let library=library.attach(&scope,library.revision,&r("two"),&r("source"),&bounds()).unwrap();
    let selection=ProfileSelection { owner_scope:scope.clone(), profile_ref:r("private"), selected_groups:BTreeSet::from([r("one"),r("two")]), selected_sources:BTreeSet::new() };
    let selected=library.selected_sources(&selection,&BTreeSet::from([r("source")])).unwrap();
    assert_eq!(library.acquisition_sources(&selected).len(),1);
    let before=library.sources.clone();
    let library=library.remove_group(&scope,library.revision,&r("one")).unwrap();
    assert_eq!(library.sources,before); assert_eq!(library.memberships.len(),1);
    let library=library.detach(&scope,library.revision,&r("two"),&r("source")).unwrap();
    assert_eq!(library.sources,before); assert!(library.memberships.is_empty());
}
#[test]
fn foreign_scope_and_stale_revision_cannot_change_or_count_sources() {
    let library=library(); let scope=library.owner_scope.clone();
    assert!(matches!(library.attach(&ScopeRef("other".into()),library.revision,&r("one"),&r("source"),&bounds()),Err(Error::Denied)));
    assert!(matches!(library.attach(&scope,Revision(1),&r("one"),&r("source"),&bounds()),Err(Error::Conflict)));
    let selection=ProfileSelection { owner_scope:scope.clone(), profile_ref:r("private"), selected_groups:BTreeSet::new(), selected_sources:BTreeSet::from([r("source")]) };
    assert!(library.selected_sources(&selection,&BTreeSet::new()).unwrap().is_empty());
    let paused=library.set_paused(&scope,library.revision,&r("source"),true).unwrap();
    assert!(paused.acquisition_sources(&BTreeSet::from([r("source")])).is_empty());
    assert_eq!(library.sources[&r("source")].paused,false);
}
#[test]
fn item_grouping_preserves_each_legacy_read_and_bookmark_value() {
    let item_id="00000000-0000-0000-0000-000000000001".parse().unwrap(); let scope=ScopeRef("view".into());
    let appearance=SourceAppearance { appearance_ref:r("appearance"), item_id, source_ref:r("source"), access_partition:ScopeRef("public".into()),
        provider_entry_key:"entry".into(), first_seen_at:None,last_seen_at:None,latest_observation_id:None,source_state:SourceState::Active,common:Extensions::default() };
    let first=AppearanceState { state_ref:r("state1"),item_id,appearance_ref:r("appearance"),legacy_item_ref:Some(r("legacy1")),scope:scope.clone(),read:true,bookmarked:false,legacy_tag_refs:vec![],revision:Revision(1),common:Extensions::default() };
    let mut second=first.clone(); second.state_ref=r("state2"); second.legacy_item_ref=Some(r("legacy2")); second.read=false;second.bookmarked=true;
    let grouped=group_items(&scope,&BTreeSet::from([r("source")]),&BTreeSet::from([ScopeRef("public".into())]),&[appearance.clone()],&[first.clone(),second.clone()]);
    assert_eq!(grouped.len(),1); assert_eq!(grouped[0].states,vec![first.clone(),second]);
    let changed=update_state(&first,&scope,Revision(1),Some(false),None).unwrap();
    assert!(!changed.read); assert!(!changed.bookmarked); assert_eq!(changed.revision,Revision(2));
    assert!(group_items(&scope,&BTreeSet::from([r("source")]),&BTreeSet::new(),&[appearance],&[first]).is_empty());
}

#[test]
fn source_edit_preserves_identity_groups_and_rejects_stale_or_foreign_edits() {
    let library=library(); let scope=library.owner_scope.clone();
    let library=library.attach(&scope,library.revision,&r("one"),&r("source"),&bounds()).unwrap();
    let before=library.sources[&r("source")].clone();
    let changed=library.edit_source(&scope,library.revision,&r("source"),before.revision,"Renamed",&bounds()).unwrap();
    let after=&changed.sources[&r("source")];
    assert_eq!(after.display_name,"Renamed"); assert_eq!(after.source_ref,before.source_ref);
    assert_eq!(after.original_url,before.original_url); assert_eq!(after.provider,before.provider);
    assert_eq!(changed.memberships,library.memberships);
    assert!(matches!(changed.edit_source(&scope,changed.revision,&r("source"),before.revision,"Another",&bounds()),Err(Error::Conflict)));
    assert!(matches!(changed.edit_source(&ScopeRef("foreign".into()),changed.revision,&r("source"),after.revision,"Another",&bounds()),Err(Error::Denied)));
}

#[test]
fn member_rules_preserve_independent_keywords_priority_and_cleanup() {
    let library=library(); let scope=library.owner_scope.clone();
    let limits=RuleLimits { max_keywords_per_source:20,max_keyword_characters:100,max_account_keywords:2 };
    let library=library.attach(&scope,library.revision,&r("one"),&r("source"),&bounds()).unwrap();
    let library=library.attach(&scope,library.revision,&r("two"),&r("source"),&bounds()).unwrap();
    let library=library.set_member_rule(&scope,library.revision,&r("one"),&r("source"),MemberRule { keywords:vec![" Science ".into()],prioritize:true },&bounds(),limits).unwrap();
    let library=library.set_member_rule(&scope,library.revision,&r("two"),&r("source"),MemberRule { keywords:vec!["music".into()],prioritize:false },&bounds(),limits).unwrap();
    assert_eq!(library.member_rules[&(r("one"),r("source"))].keywords,vec!["Science"]);
    assert!(library.member_rules[&(r("one"),r("source"))].prioritize);
    assert!(!library.member_rules[&(r("two"),r("source"))].prioritize);
    assert!(matches!(library.set_member_rule(&scope,library.revision,&r("one"),&r("source"),MemberRule { keywords:vec!["Science".into(),"science".into()],prioritize:true },&bounds(),limits),Err(Error::Invalid { .. })));
    assert!(matches!(library.set_member_rule(&scope,library.revision,&r("one"),&r("source"),MemberRule { keywords:vec!["a".into(),"b".into()],prioritize:true },&bounds(),limits),Err(Error::LimitExceeded { .. })));
    let library=library.detach(&scope,library.revision,&r("one"),&r("source")).unwrap();
    assert_eq!(library.member_rules.len(),1); assert!(library.member_rules.contains_key(&(r("two"),r("source"))));
    assert_eq!(library.sources.len(),1);
}
