//! How an instance sits in the Folio document: one layer of kind `component_instance` whose payload
//! is a `PrimitiveReference` to the `SCIN-*` instance record. Folio stays the single document; the
//! instance record, definitions and variables are plain values owned by the host (STU-CON-008).
use crate::R;
use crate::instance_resolution::check_id;
use hsk_studio_accord::SCHEMA_STUDIO_COMPONENT_INSTANCE;
use hsk_studio_folio::{LayerSchema, Parent, Payload, StudioLayer};

/// `(kind, schema_id)` the host `Resolver::primitive_layer_binding` must accept for instance layers.
pub const INSTANCE_LAYER_BINDING: (&str, &str) =
    ("component_instance", SCHEMA_STUDIO_COMPONENT_INSTANCE);

/// Builds the Folio layer for an instance. `layer_id` is `SLYR-*`, `instance_id` is `SCIN-*`.
pub fn instance_layer(
    layer_id: &str,
    name: &str,
    parent: Parent,
    instance_id: &str,
) -> R<StudioLayer> {
    check_id(layer_id, "SLYR")?;
    check_id(instance_id, "SCIN")?;
    Ok(StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: layer_id.to_owned(),
        name: name.to_owned(),
        parent,
        kind: INSTANCE_LAYER_BINDING.0.to_owned(),
        payload: Payload::PrimitiveReference {
            schema_id: INSTANCE_LAYER_BINDING.1.to_owned(),
            object_id: instance_id.to_owned(),
        },
        visible: true,
        appearance: None,
        text_frame: None,
    })
}
