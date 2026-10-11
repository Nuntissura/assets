use cabinet::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("pass a host-owned external settings JSON path")?;
    let snapshot = Snapshot {
        context: Context {
            scope: "settings-demo".into(),
            generation: 1,
            revision: 1,
        },
        entries: vec![Entry::new("projects", None, "Projects", EntryKind::Folder)],
        hierarchy_complete: true,
    };
    let mut tree = Cabinet::new(snapshot.clone())?;
    tree.set_sort(Sort::NameAscending);
    tree.set_folder_sort("projects", Some(Sort::ModifiedDescending))?;
    let mut settings = tree.settings().clone();
    settings.peak.expand_delay_ms = 1600;
    tree.apply_settings(settings)?;
    // Demonstration host persistence. Production hosts choose scoped storage and atomic save policy.
    std::fs::write(&path, serde_json::to_vec_pretty(tree.settings())?)?;
    let loaded: Settings = serde_json::from_slice(&std::fs::read(&path)?)?;
    let mut reopened = Cabinet::new(snapshot)?;
    reopened.apply_settings(loaded)?;
    assert_eq!(reopened.settings(), tree.settings());
    reopened.set_sort(Sort::FileSizeDescending);
    assert_eq!(
        reopened.effective_sort(Some("projects")),
        Sort::ModifiedDescending
    );
    println!("Settings survived a file save/reload; folder override survived tree-default change");
    Ok(())
}
