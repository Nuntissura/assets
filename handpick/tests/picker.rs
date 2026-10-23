use handpick::*;

fn item(id: &str, category: ResultCategory) -> PickerItem {
    PickerItem { item: SearchItem { key: ResultKey { provider: ProviderId::new("owner").unwrap(), result: ResultId::new(id).unwrap() },
        label: format!("Name {id}"), detail: None, actions: vec![] }, category, location: None, highlights: vec![], preview: None }
}
fn coverage(category: ResultCategory, loaded: u32, total: Option<u32>) -> PickerCoverage {
    PickerCoverage { category, status: DeliveryStatus::Complete, loaded, total }
}

#[test]
fn picker_groups_preview_three_expand_collapse_and_skip_headings() {
    let mut picker = Picker::new();
    let mut rows: Vec<_> = (0..5).map(|i| item(&i.to_string(), ResultCategory::Notes)).collect();
    rows.push(item("file", ResultCategory::Files));
    picker.set_results(rows, vec![coverage(ResultCategory::Files, 1, Some(1)), coverage(ResultCategory::Notes, 5, Some(8))]).unwrap();
    let view = picker.view();
    assert_eq!(view.groups[0].category, ResultCategory::Notes);
    assert_eq!(view.groups[0].shown, 3);
    assert!(view.groups[0].has_more);
    picker.move_selection(3);
    assert_eq!(picker.view().selected.unwrap().result.as_str(), "file");
    picker.show_more(ResultCategory::Notes);
    assert_eq!(picker.view().groups[0].shown, 5);
    assert_eq!(picker.view().selected.unwrap().result.as_str(), "file");
    picker.toggle(ResultCategory::Files);
    assert_eq!(picker.view().selected.unwrap().result.as_str(), "0");
    picker.toggle(ResultCategory::Notes);
    assert!(picker.view().selected.is_none());
    assert_eq!(picker.view().groups[0].loaded, 5);
    assert_eq!(picker.view().groups[0].total, Some(8));
}

#[test]
fn picker_result_identity_survives_reordering_and_hidden_rows_cannot_select() {
    let mut picker = Picker::new();
    let rows: Vec<_> = (0..4).map(|i| item(&i.to_string(), ResultCategory::Notes)).collect();
    picker.set_results(rows.clone(), vec![coverage(ResultCategory::Notes, 4, None)]).unwrap();
    picker.select(rows[1].item.key.clone()).unwrap();
    assert_eq!(picker.select(rows[3].item.key.clone()), Err(PickerError::MissingResult));
    picker.update(vec![rows[2].clone(), rows[1].clone(), rows[0].clone()], vec![coverage(ResultCategory::Notes, 3, None)]).unwrap();
    assert_eq!(picker.view().selected, Some(rows[1].item.key.clone()));
    picker.move_selection(i32::MAX);
    assert_eq!(picker.view().selected, Some(rows[0].item.key.clone()));
    picker.move_selection(i32::MIN);
    assert_eq!(picker.view().selected, Some(rows[2].item.key.clone()));
}

#[test]
fn picker_malformed_replacement_preserves_prior_state() {
    let mut picker = Picker::new();
    let row = item("one", ResultCategory::Notes);
    picker.set_results(vec![row.clone()], vec![coverage(ResultCategory::Notes, 1, Some(1))]).unwrap();
    let before = picker.view();
    assert_eq!(picker.set_results(vec![row.clone(), row.clone()], vec![coverage(ResultCategory::Notes, 2, Some(2))]), Err(PickerError::DuplicateResult));
    assert_eq!(picker.view(), before);
    assert_eq!(picker.set_results(vec![row.clone()], vec![coverage(ResultCategory::Notes, 2, Some(2))]), Err(PickerError::InvalidCoverage));
    assert_eq!(picker.view(), before);
    let mut invalid = row.clone();
    invalid.preview = Some(PreviewReference { kind: PreviewKind::Image, owner_ref: "https://untrusted.example/image".into() });
    assert_eq!(picker.set_results(vec![invalid], vec![coverage(ResultCategory::Notes, 1, None)]), Err(PickerError::InvalidPreview));
    assert_eq!(picker.view(), before);
    let mut invalid = row;
    invalid.item.label = "猫".into(); invalid.highlights = vec![ByteSpan { start: 1, end: 3 }];
    assert_eq!(picker.set_results(vec![invalid], vec![coverage(ResultCategory::Notes, 1, None)]), Err(PickerError::InvalidHighlight));
    assert_eq!(picker.view(), before);
}

#[test]
fn picker_coverage_status_unknown_totals_and_preview_roundtrip_are_preserved() {
    let mut picker = Picker::new();
    let mut row = item("image", ResultCategory::Files);
    row.preview = Some(PreviewReference { kind: PreviewKind::Image, owner_ref: "thumbnail-17".into() });
    row.location = Some("Pictures".into());
    picker.set_results(vec![row], vec![PickerCoverage { category: ResultCategory::Files, status: DeliveryStatus::Partial, loaded: 1, total: None }]).unwrap();
    let view = picker.view();
    assert_eq!(view.groups[0].status, DeliveryStatus::Partial);
    assert_eq!(view.groups[0].total, None);
    let restored: PickerView = serde_json::from_str(&serde_json::to_string(&view).unwrap()).unwrap();
    assert_eq!(restored, view);
}
