mod fixtures;

fn main() {
    println!(
        "{}",
        serde_json::to_string(&fixtures::comparison_cases()).unwrap()
    );
}
