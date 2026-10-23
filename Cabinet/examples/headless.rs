use cabinet::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut cabinet = Cabinet::new(Snapshot {
        context: Context {
            scope: "example-project".into(),
            generation: 1,
            revision: 1,
        },
        entries: vec![
            Entry::new("src", None, "Source", EntryKind::Folder),
            Entry::new("docs", None, "Documents", EntryKind::Folder),
            Entry::new("readme", Some("src".into()), "README.md", EntryKind::Leaf),
        ],
        hierarchy_complete: true,
    })?;
    cabinet.reveal("readme")?;
    let before = cabinet.snapshot().clone();
    let request = cabinet.request(Operation::Move {
        ids: vec!["readme".into()],
        placement: Placement::Into(Some("docs".into())),
    })?;
    // Demonstration host only: an application must recheck authority and reread durable state here.
    let mut after = before.clone();
    after.context.revision += 1;
    let moved = after
        .entries
        .iter_mut()
        .find(|entry| entry.id == "readme")
        .unwrap();
    moved.parent = Some("docs".into());
    moved.revision += 1;
    cabinet.complete(Completion {
        request_id: request.id,
        context: request.context,
        result: Ok(after),
        undo_token: Some("demo-host:restore-location:1".into()),
    })?;
    assert_eq!(
        cabinet.entry("readme").unwrap().parent.as_deref(),
        Some("docs")
    );
    let undo = cabinet.request_undo()?;
    let mut restored = before;
    restored.context.revision = 3;
    for entry in &mut restored.entries {
        entry.revision = cabinet.entry(&entry.id).unwrap().revision + 1;
    }
    cabinet.complete(Completion {
        request_id: undo.id,
        context: undo.context,
        result: Ok(restored),
        undo_token: Some("demo-host:redo-location:1".into()),
    })?;
    assert_eq!(
        cabinet.entry("readme").unwrap().parent.as_deref(),
        Some("src")
    );
    println!(
        "Host-confirmed leaf move and undo succeeded; {} visible rows; redo available: {}",
        cabinet.rows().len(),
        cabinet.can_redo()
    );
    Ok(())
}
