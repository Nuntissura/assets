use fforager_transport::{
    run_ordinary_transport_decision, validate_ordinary_transport_decision_report,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("fforager-ordinary-transport-decision: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    if std::env::args_os().len() != 1 {
        return Err("usage: fforager-ordinary-transport-decision".to_owned());
    }
    let report = run_ordinary_transport_decision().map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(&report).map_err(|error| error.to_string())?;
    validate_ordinary_transport_decision_report(&bytes).map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}
