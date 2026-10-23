//! Consumer example (headless): `--descriptor` prints the crate descriptor; `--text <value>`
//! runs typed entry against the opacity fixture; no flag prints the agent JSON projection.
use hsk_studio_controls::projection::agent_json;
use hsk_studio_controls::{ControlSpec, NumberSpec, Registry, ValueStore};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--descriptor") {
        println!("{}", hsk_studio_controls::DESCRIPTOR);
        return;
    }
    let mut registry = Registry::new();
    let spec = ControlSpec::number(
        "layer.opacity",
        "Opacity",
        "How much of the layer shows through.",
        NumberSpec::slider(2, (0.0, 1.0), (0.2, 0.8)),
        0.5,
    );
    registry.register(spec.clone()).expect("fixture descriptor is valid");
    registry.register_consumer("layer.opacity", "example.consumer").expect("control is registered");
    if args.first().map(String::as_str) == Some("--text") {
        let text = args.get(1).map_or("", String::as_str);
        match spec.parse_text(text) {
            Ok(v) => println!("accepted {v:?} (soft state {:?})", spec.soft_state(&v)),
            Err(e) => println!("rejected [{}]: {e}", e.code()),
        }
        return;
    }
    println!("{}", agent_json(&registry, Some(&ValueStore::new(&registry))));
}
