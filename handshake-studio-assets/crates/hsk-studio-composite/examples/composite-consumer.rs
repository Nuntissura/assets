use hsk_studio_composite::blend::MODE_TABLE;
use std::env;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args == ["--modes"] {
        for row in MODE_TABLE {
            println!(
                "{:>2} {:<18} {:<11} {}",
                row.mode.discriminant(),
                row.key,
                row.fidelity.key(),
                row.reference
            );
        }
        return;
    }
    eprintln!("usage: composite-consumer --modes");
    std::process::exit(2);
}
