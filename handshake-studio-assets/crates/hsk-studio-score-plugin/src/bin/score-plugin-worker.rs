//! Worker process of `hsk-studio-score-plugin`: spawned and supervised by `IsolatedHost`, speaks the
//! framed protocol on stdin/stdout and exits when stdin closes. The reference backend only; it is not
//! meant to be run by hand.
fn main() {
    std::process::exit(hsk_studio_score_plugin::worker_main());
}
