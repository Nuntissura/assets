//! Lane SMB-COMMAND-SURFACE-1: the descriptor catalog keeps [STU-MDL-100] ids and [STU-SHL-243] aliases.

use hsk_studio_controls::{UiCatalog, UiCatalogError, UiDescriptor, UiKind, kebab, stable_id, valid_author_id};

fn command(author_id: &str) -> UiDescriptor {
    UiDescriptor::new(author_id.to_string(), UiKind::Command, "A command", "layer")
}

#[test]
fn the_author_id_grammar_is_closed() {
    for good in ["studio.tool.draw-path.pen", "studio.options.pen.width", "studio.panel.properties.stroke.width", "studio.tab.layers", "studio.panel.preset-library#2"] {
        assert!(valid_author_id(good), "{good}");
    }
    for bad in ["", "studio", "studio.tab", "tool.pen.brush", "studio.tab.Layers", "studio.tab.a_b", "studio..layers", "studio.tab.layers#", "studio.tab.layers#x", "studio.tab.layers.", "studio.tab.la yers"] {
        assert!(!valid_author_id(bad), "{bad}");
    }
    assert_eq!(kebab("layer.set_visible"), "layer.set-visible");
    assert_eq!(stable_id("studio.tool.draw-path.pen"), "hs-studio-tool-draw-path-pen");
}

#[test]
fn an_id_resolves_to_one_element_and_a_collision_is_refused_whole() {
    let mut c = UiCatalog::new();
    c.insert(command("studio.command.layer.rename").with_command("layer.rename").with_alias("old.rename")).unwrap();
    assert_eq!(c.resolve("old.rename").unwrap().author_id, "studio.command.layer.rename");
    assert_eq!(c.resolve("layer.rename").unwrap().author_id, "studio.command.layer.rename");
    assert_eq!(c.resolve("studio.command.layer.rename").unwrap().command_id.as_deref(), Some("layer.rename"));
    assert!(c.resolve("no.such.id").is_none());

    let count = c.rows().len();
    assert_eq!(c.insert(command("studio.command.layer.rename")).unwrap_err().code(), "ui_author_id_duplicate");
    assert_eq!(c.insert(command("bad id")), Err(UiCatalogError::InvalidAuthorId("bad id".to_string())));
    let taken = c.insert(command("studio.command.layer.other").with_alias("new.other").with_alias("old.rename")).unwrap_err();
    assert_eq!(taken, UiCatalogError::AliasCollision { id: "old.rename".to_string(), holder: "studio.command.layer.rename".to_string() });
    assert!(c.resolve("new.other").is_none(), "a refused descriptor leaves nothing behind");
    // A live id may not also be someone's alias, either way round.
    assert_eq!(c.insert(command("studio.command.layer.third").with_alias("studio.command.layer.rename")).unwrap_err().code(), "ui_alias_collision");
    assert_eq!(c.insert(command("studio.command.layer.self").with_alias("studio.command.layer.self")).unwrap_err().code(), "ui_alias_collision");
    assert_eq!(c.insert(command("studio.command.layer.twice").with_alias("x.y").with_alias("x.y")).unwrap_err().code(), "ui_alias_collision");
    assert_eq!(c.rows().len(), count);
    let json = c.json();
    assert_eq!(json["count"], 1);
    assert_eq!(json["descriptors"][0]["stable_id"], "hs-studio-command-layer-rename");
}
