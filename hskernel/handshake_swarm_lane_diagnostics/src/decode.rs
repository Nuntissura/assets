use crate::model::SwarmLaneDiagnosticsProjection;
use crate::validation::validate_projection_for_native_surface;

/// Decode the exact backend wire shape before validating its native surface semantics.
pub fn decode_projection_for_native_surface(
    raw: serde_json::Value,
) -> Result<SwarmLaneDiagnosticsProjection, String> {
    let projection: SwarmLaneDiagnosticsProjection =
        serde_json::from_value(raw.clone()).map_err(|error| error.to_string())?;
    let serialized = serde_json::to_value(&projection)
        .map_err(|error| format!("native projection reserialization failed: {error}"))?;
    if serialized != raw {
        return Err("backend/native projection JSON field shape changed or lost data".to_owned());
    }
    validate_projection_for_native_surface(&projection)?;
    Ok(projection)
}

#[cfg(test)]
mod tests {
    use super::decode_projection_for_native_surface;
    use crate::model::test_projection;

    #[test]
    fn decoder_accepts_complete_valid_projection() {
        let projection = test_projection();
        let wire = serde_json::to_value(&projection).expect("serialize pure unit projection");
        assert_eq!(
            decode_projection_for_native_surface(wire).expect("valid pure unit projection"),
            projection,
        );
    }

    #[test]
    fn decoder_rejects_extra_wire_field_without_silent_loss() {
        let mut wire = serde_json::to_value(test_projection()).expect("serialize pure unit projection");
        wire.as_object_mut().expect("projection object")
            .insert("unit_extra_field".into(), serde_json::Value::Bool(true));
        assert_eq!(
            decode_projection_for_native_surface(wire).expect_err("extra field"),
            "backend/native projection JSON field shape changed or lost data",
        );
    }

    #[test]
    fn decoder_rejects_missing_required_run() {
        let mut wire = serde_json::to_value(test_projection()).expect("serialize pure unit projection");
        wire.as_object_mut().expect("projection object").remove("run");
        assert_eq!(
            decode_projection_for_native_surface(wire).expect_err("missing run"),
            "missing field `run`",
        );
    }

    #[test]
    fn decoder_preserves_schema_validation_error() {
        let mut projection = test_projection();
        projection.schema_id = "unit.invalid.schema".into();
        let wire = serde_json::to_value(projection).expect("serialize pure unit projection");
        assert_eq!(
            decode_projection_for_native_surface(wire).expect_err("wrong schema"),
            "schema_id mismatch: unit.invalid.schema",
        );
    }
}
