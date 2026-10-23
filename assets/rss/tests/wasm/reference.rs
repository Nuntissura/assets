mod fixtures;
fn main() {
    println!(
        "{}",
        serde_json::to_string(&fixtures::scenarios()).expect("native bridge fixtures")
    );
}
