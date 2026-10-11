//! A control panel bound to one layer, built ONLY from `hsk-studio-controls` descriptors. The same
//! registry feeds the agent JSON, the manual topics and (in a host) the egui renderer; the harness
//! renders nothing. Each control carries an explicit commit route: only name and visibility have a
//! chronicle property today, so layer appearance values are projected read-only and an edit is
//! refused with a typed reason rather than pretending to commit.

use hsk_studio_controls::{
    ChangeSource, ChoiceOption, ControlError, ControlSpec, IntegerSpec, NumberSpec, Registry, SectionSpec, Value,
    ValueStore,
};
use hsk_studio_folio::{Isolation, LayerAppearance, StudioLayer};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

pub const NAME: &str = "layer.name";
pub const VISIBLE: &str = "layer.visible";
pub const OPACITY: &str = "appearance.opacity";
pub const FILL_OPACITY: &str = "appearance.fill_opacity";
pub const BLEND_MODE: &str = "appearance.blend_mode";
pub const CLIPPED: &str = "appearance.clipped";
pub const ISOLATION: &str = "appearance.isolation";

/// Where an accepted control edit is committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Chronicle `name` property of the layer (provider `chronicle.patch`).
    ChronicleName,
    /// Chronicle `visible` property of the layer (provider `chronicle.patch`).
    ChronicleVisible,
    /// No commit route exists; the reason is a stable code.
    None(&'static str),
}

impl Route {
    pub fn to_json(self) -> Json {
        match self {
            Route::ChronicleName => json!({"route":"chronicle.patch","property":"name"}),
            Route::ChronicleVisible => json!({"route":"chronicle.patch","property":"visible"}),
            Route::None(reason) => json!({"route":"none","reason":reason}),
        }
    }
}

/// The values a panel shows for one layer.
#[derive(Clone, Debug, PartialEq)]
pub struct PanelValues {
    pub name: String,
    pub visible: bool,
    pub group: bool,
    pub appearance: LayerAppearance,
}

impl PanelValues {
    pub fn of(layer: &StudioLayer) -> Self {
        let group = layer.kind == "group";
        Self { name: layer.name.clone(), visible: layer.visible, group, appearance: layer.effective_appearance() }
    }

    /// Identity values (used for the manual, which documents the panel without a document).
    pub fn sample() -> Self {
        Self { name: "Layer".into(), visible: true, group: false, appearance: LayerAppearance::identity(false) }
    }
}

fn isolation_id(i: Isolation) -> &'static str {
    match i {
        Isolation::PassThrough => "pass_through",
        Isolation::Isolated => "isolated",
    }
}

pub struct LayerPanel {
    pub registry: Registry,
    pub store: ValueStore,
    pub routes: BTreeMap<String, Route>,
}

/// Builds the descriptors and a store holding the layer's current values.
pub fn build(values: &PanelValues) -> Result<LayerPanel, ControlError> {
    let identity = LayerAppearance::identity(values.group);
    let a = &values.appearance;
    let mut registry = Registry::new();
    registry.register(ControlSpec::text(NAME, "Name", "Display name of the layer.", 256, false, &values.name))?;
    registry.register(ControlSpec::toggle(VISIBLE, "Visible", "Shows or hides the layer in the composition.", values.visible))?;
    registry.register_section(SectionSpec::new(
        "appearance",
        "Appearance",
        "How the layer composites: opacity, fill opacity, blend mode, clipping and isolation.",
    ))?;
    let percent = || NumberSpec::slider(1, (0.0, 100.0), (0.0, 100.0));
    registry.register(
        ControlSpec::number(OPACITY, "Opacity", "Fades the whole layer including its effects.", percent(), identity.opacity)
            .with_unit("%")
            .in_group(&["appearance"]),
    )?;
    registry.register(
        ControlSpec::number(
            FILL_OPACITY,
            "Fill opacity",
            "Fades the layer's pixel or fill content but not its effects.",
            percent(),
            identity.fill_opacity,
        )
        .with_unit("%")
        .in_group(&["appearance"]),
    )?;
    registry.register(
        ControlSpec::integer(
            BLEND_MODE,
            "Blend mode",
            "Stable blend-mode discriminant, 1 to 35.",
            IntegerSpec::field(Some(1), Some(35)),
            i64::from(identity.blend_mode),
        )
        .in_group(&["appearance"]),
    )?;
    registry.register(
        ControlSpec::toggle(CLIPPED, "Clipped", "Clips the layer to the base layer below it.", identity.clipped)
            .in_group(&["appearance"]),
    )?;
    registry.register(
        ControlSpec::choice(
            ISOLATION,
            "Isolation",
            "Whether the layer composites on its own (isolated) or passes through to the backdrop.",
            vec![ChoiceOption::new("pass_through", "Pass through"), ChoiceOption::new("isolated", "Isolated")],
            isolation_id(identity.isolation),
        )
        .in_group(&["appearance"]),
    )?;
    let mut routes = BTreeMap::new();
    for (id, route, consumer) in [
        (NAME, Route::ChronicleName, "harness.commit.chronicle"),
        (VISIBLE, Route::ChronicleVisible, "harness.commit.chronicle"),
        (OPACITY, Route::None("no_chronicle_property"), "harness.appearance-readback"),
        (FILL_OPACITY, Route::None("no_chronicle_property"), "harness.appearance-readback"),
        (BLEND_MODE, Route::None("no_chronicle_property"), "harness.appearance-readback"),
        (CLIPPED, Route::None("no_chronicle_property"), "harness.appearance-readback"),
        (ISOLATION, Route::None("no_chronicle_property"), "harness.appearance-readback"),
    ] {
        registry.register_consumer(id, consumer)?;
        routes.insert(id.to_string(), route);
    }
    let mut store = ValueStore::new(&registry);
    for (id, value) in [
        (OPACITY, Value::Number(a.opacity)),
        (FILL_OPACITY, Value::Number(a.fill_opacity)),
        (BLEND_MODE, Value::Int(i64::from(a.blend_mode))),
        (CLIPPED, Value::Bool(a.clipped)),
        (ISOLATION, Value::Choice(isolation_id(a.isolation).to_string())),
    ] {
        store.set(&registry, id, value, ChangeSource::Model, None)?;
    }
    Ok(LayerPanel { registry, store, routes })
}

impl LayerPanel {
    /// Agent projection: the controls crate's JSON plus each control's commit route.
    pub fn to_json(&self, layer_id: &str) -> Json {
        let controls: Json =
            serde_json::from_str(&hsk_studio_controls::projection::agent_json(&self.registry, Some(&self.store)))
                .unwrap_or(Json::Null);
        let bindings: serde_json::Map<String, Json> = self.routes.iter().map(|(k, r)| (k.clone(), r.to_json())).collect();
        json!({"layer_id":layer_id,"controls":controls,"bindings":bindings,"audit":self.registry.audit().to_string()})
    }
}
