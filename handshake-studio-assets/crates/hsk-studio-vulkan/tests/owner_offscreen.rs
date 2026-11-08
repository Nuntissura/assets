//! The transient owner on the real device, offscreen (no surface extension, no window): clear and CPU-buffer upload
//! read back exactly, one device per process, typed refusals. GPU test group; validation on in debug builds when the
//! loader lists the Khronos layer, and zero validation errors.
use hsk_studio_vulkan::{GpuClientPort, Issuer, Owner, OwnerConfig, Refusal, VALIDATION_LAYER, process_validation, rgba_len};

#[test]
fn offscreen_clear_upload_readback_exact_on_one_owner() {
    let mut owner = Owner::open(OwnerConfig::default()).expect("Vulkan 1.3 device on this machine");
    let info = owner.device_info().clone();
    assert!(!info.name.is_empty());
    assert!(info.api_version[0] == 1 && info.api_version[1] >= 3, "{:?}", info.api_version);
    assert_ne!(info.device_type, "cpu");
    assert_eq!(info.device_uuid.len(), 32);
    // Item V: debug builds enable the Khronos layer when the loader lists it; release builds never do.
    let listed = owner.layers().present.iter().any(|l| l == VALIDATION_LAYER);
    let want = if !cfg!(debug_assertions) {
        "off"
    } else if listed {
        "on"
    } else {
        "absent"
    };
    assert_eq!(owner.validation(), want, "listed {listed}, layers {:?}", owner.layers());
    assert_eq!(owner.validation_report().state, want);
    assert_eq!(owner.layers().disabled, "~implicit~");
    let enabled: &[&str] = if want == "on" { &[VALIDATION_LAYER] } else { &[] };
    assert_eq!(owner.layers().enabled, enabled);
    println!("device {} driver {} {} validation {want}", info.name, info.driver_name, info.driver_info);

    // One device per process.
    assert_eq!(Owner::open(OwnerConfig::default()).err(), Some(Refusal::OwnerAlreadyOpen));

    let lease = owner.lease();
    assert_eq!(lease.issuer, Issuer::TransientAssetLocal);
    assert!(lease.instance != 0 && lease.device != 0 && lease.physical_device != 0 && lease.get_instance_proc_addr != 0);
    assert!(lease.extensions.is_empty(), "offscreen owner enables no surface extension");

    let (w, h) = (64u32, 48u32);
    let pane = owner.create_pane(w, h).unwrap();
    assert_eq!(owner.pane_extent(pane), Ok((w, h)));
    assert_eq!(owner.read_pane(pane), Err(Refusal::InvalidPane), "never-written pane is not read");

    let clear = [0x2b, 0x6c, 0xb0, 0xff];
    owner.clear_pane(pane, clear).unwrap();
    let back = owner.read_pane(pane).unwrap();
    assert_eq!(back.len(), rgba_len(w, h).unwrap());
    assert!(back.chunks_exact(4).all(|p| p == clear), "clear value round-trips exactly");

    let painted: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, (i / 7 % 256) as u8, (i * 3 % 256) as u8, 255]).collect();
    owner.upload_pane(pane, &painted).unwrap();
    assert_eq!(owner.read_pane(pane).unwrap(), painted, "CPU-painted buffer round-trips exactly");

    assert_eq!(owner.upload_pane(pane, &painted[..8]), Err(Refusal::InvalidBuffer));
    assert_eq!(owner.create_pane(0, 4).err(), Some(Refusal::InvalidExtent));
    assert_eq!(owner.create_pane(20_000, 4).err(), Some(Refusal::InvalidExtent));
    owner.release_pane(pane).unwrap();
    assert_eq!(owner.read_pane(pane), Err(Refusal::InvalidPane));
    drop(owner);

    // The flag is released with the device: a later owner in the same process opens.
    let again = Owner::open(OwnerConfig::default()).expect("re-open after drop");
    assert_eq!(again.validation(), want);
    drop(again);
    assert_validation_clean(want);
}

/// Every GPU test ends here: with the layer on, no error over the whole process (destruction included); warnings
/// are printed for the lane file's rulings.
fn assert_validation_clean(want: &str) {
    let v = process_validation();
    assert_eq!(v.state, want, "{v:?}");
    println!("validation {} errors {} warnings {} first {:?}", v.state, v.errors, v.warnings, v.first);
    if v.state == "on" {
        assert_eq!(v.errors, 0, "validation errors: {v:?}");
    } else {
        println!("NOT_PROVEN: validation layer {VALIDATION_LAYER} (state {})", v.state);
    }
}
