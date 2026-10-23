use kumihimo_core::{Core, Identity, Limits, RestoreOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A production host supplies incarnation uniqueness; these are distinct example tokens.
    let core = Core::new(
        Identity::new("example-layout", "example-process-1")?,
        Limits::default(),
        16,
    )?;
    let saved = kumihimo_layout_json::encode_core(&core, Default::default())?;
    assert!(saved.is_current(&core));
    let dto = kumihimo_layout_json::decode(saved.bytes())?;
    let restored = Core::from_persisted(
        dto,
        "example-process-2".into(),
        RestoreOptions::default(),
        16,
    )?;
    assert_eq!(restored.snapshot().generation, 1);
    assert_eq!(restored.ledger_status().high_watermark.sequence(), 0);
    println!(
        "Schema 1 JSON roundtrip: {} bytes; fresh generation 1",
        saved.bytes().len()
    );
    Ok(())
}
