use crate::*;

fn accept_runtime_ref(_: &str, _: &str) -> Result<(), &'static str> {
    Ok(())
}

#[test]
fn ckc_source_ref_readouts_preserve_existing_wire_values() {
    for (kind, value, token) in [
        (
            CkcSourceRefKind::Folder,
            "atelier://folder/mt035-primary-link",
            "folder",
        ),
        (
            CkcSourceRefKind::SourceUrl,
            "https://example.invalid/mt035-primary-link",
            "source_url",
        ),
    ] {
        let readout = ckc_source_ref_readout(kind, value, &mut accept_runtime_ref).unwrap();
        assert_eq!(kind.ref_kind(), token);
        assert_eq!(serde_json::to_value(kind).unwrap(), token);
        assert_eq!(
            serde_json::from_value::<CkcSourceRefKind>(serde_json::json!(token)).unwrap(),
            kind
        );
        assert_eq!(
            serde_json::to_value(readout).unwrap(),
            serde_json::json!({ "ref_kind": token, "value": value })
        );
    }
}

#[test]
fn ckc_source_refs_reject_existing_blank_and_padded_inputs_before_host_check() {
    let mut calls = 0;
    let mut checker = |_: &str, _: &str| -> Result<(), &'static str> {
        calls += 1;
        Ok(())
    };
    for value in ["", " source-url://atelier/padded"] {
        assert_eq!(
            normalize_optional_ckc_source_ref(
                CkcSourceRefKind::SourceUrl,
                "source_url_ref",
                &Some(value.to_owned()),
                &mut checker,
            ),
            Err(CkcSourceRefError::Validation(
                "source_url_ref must not be empty or padded".to_owned()
            ))
        );
    }
    assert_eq!(calls, 0);
}

#[test]
fn ckc_source_refs_preserve_existing_kind_rejections() {
    for (kind, field, value, message) in [
        (
            CkcSourceRefKind::Folder,
            "source_path_ref",
            "https://example.invalid/not-a-folder",
            "source_path_ref must be a folder/source-path ref, not a source URL ref",
        ),
        (
            CkcSourceRefKind::SourceUrl,
            "source_url_ref",
            "atelier://folder/not-a-source-url",
            "source_url_ref must be an http(s) or source-url:// ref",
        ),
    ] {
        assert_eq!(
            validate_ckc_source_ref(kind, field, value, &mut accept_runtime_ref),
            Err(CkcSourceRefError::Validation(message.to_owned()))
        );
    }
}

#[test]
fn ckc_source_ref_boundary_preserves_host_error_and_validation_order() {
    let mut observed = Vec::new();
    let mut checker = |field: &str, value: &str| {
        observed.push((field.to_owned(), value.to_owned()));
        Err("host portable-ref rejection")
    };
    let value = "https://example.invalid/not-a-folder";
    assert_eq!(
        validate_ckc_source_ref(
            CkcSourceRefKind::Folder,
            "source_path_ref",
            value,
            &mut checker,
        ),
        Err(CkcSourceRefError::RuntimeRef("host portable-ref rejection"))
    );
    assert_eq!(observed, [("source_path_ref".to_owned(), value.to_owned())]);
}

#[test]
fn optional_source_ref_boundary_preserves_absence_and_invalid_read_redaction() {
    let mut calls = 0;
    let mut checker = |_: &str, _: &str| -> Result<(), &'static str> {
        calls += 1;
        Ok(())
    };
    assert_eq!(
        optional_ckc_source_ref_readout(CkcSourceRefKind::Folder, None, &mut checker),
        Ok(None)
    );
    assert_eq!(
        normalize_optional_ckc_source_ref(
            CkcSourceRefKind::Folder,
            "source_path_ref",
            &None,
            &mut checker,
        ),
        Ok(None)
    );
    assert_eq!(
        portable_optional_ckc_source_ref(
            CkcSourceRefKind::Folder,
            "source_path_ref",
            None,
            &mut checker,
        ),
        None
    );
    assert_eq!(calls, 0);
    assert_eq!(
        portable_optional_ckc_source_ref(
            CkcSourceRefKind::Folder,
            "source_path_ref",
            Some("https://example.invalid/not-a-folder".to_owned()),
            &mut accept_runtime_ref,
        ),
        None
    );
    assert_eq!(
        portable_optional_ckc_source_ref(
            CkcSourceRefKind::Folder,
            "source_path_ref",
            Some("atelier://folder/mt035-primary-link".to_owned()),
            &mut accept_runtime_ref,
        ),
        Some("atelier://folder/mt035-primary-link".to_owned())
    );
}
