//! Component, variant-set, component-property and instance authoring rows (lane SMB-DS-COMPONENTS-AUTHOR-1;
//! [STU-DS-107] and [STU-DS-112] command names, [STU-DS-162]): every row runs `Viewer::component_command`, one
//! undo step per mutating row. The `design.instance.*` rows of `design.rs` stay as they are (aliases are the
//! catalog's job).

use super::{CommandEntry, ParamSpec, ValueType, list as l, number as n, one_of as o, record as r, shaped, text as t, typed};

const METHOD: &str = "Viewer::component_command";
const PROPERTY_TYPES: &str = "BOOLEAN|TEXT|INSTANCE_SWAP|VARIANT|SLOT";
const DEFAULT: ParamSpec = shaped("default", ValueType::Class, "a boolean, a text or an SCMP id, by the property type");
const OPTIONS: ParamSpec = r("options", "INSTANCE_SWAP: [{type: COMPONENT|COMPONENT_SET, key}]; VARIANT: the ordered value list").opt();

pub(super) const ROWS: &[CommandEntry] = &[
    typed("component.create_from_selection", METHOD, "Create component", true, &[l("layers"), t("name").opt()]),
    typed("component.create_one_per_top_level_node", METHOD, "Create multiple components", true, &[l("layers")]),
    typed("component.create_instance", METHOD, "Create instance", true, &[t("component"), n("x"), n("y"), t("parent").opt()]),
    typed("component.list_instances", METHOD, "List instances", false, &[t("component")]),
    typed("component.add_property", METHOD, "Add component property", true, &[t("component"), t("name"), o("type", PROPERTY_TYPES), DEFAULT, OPTIONS]),
    typed("component.edit_property", METHOD, "Edit component property", true, &[t("component"), t("name"), t("new_name").opt(), DEFAULT.opt(), OPTIONS]),
    typed("component.delete_property", METHOD, "Delete component property", true, &[t("component"), t("name")]),
    typed("component.set_property_reference", METHOD, "Set property reference", true, &[t("layer"), o("field", "visible|characters"), t("property").opt()]),
    typed("component.set_description", METHOD, "Set component description", true, &[t("component"), t("description"), r("documentation_links", "[{name, url}]").opt()]),
    typed("component.sync_instances", METHOD, "Sync instances", true, &[t("component")]),
    typed("component_set.combine_as_variants", METHOD, "Combine as variants", true, &[l("components"), r("properties", "{name: [value, ...]}"), r("assignments", "{component: {name: value}}"), t("name").opt()]),
    typed("component_set.set_default_variant", METHOD, "Set default variant", true, &[t("set"), t("component")]),
    typed("instance.detach", METHOD, "Detach instance", true, &[t("layer")]),
    typed("instance.get_main_component", METHOD, "Get main component", false, &[t("layer")]),
];
