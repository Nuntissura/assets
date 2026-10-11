//! AV1 decoder worker process of `hsk-studio-reel-native` (built only with the `av1` feature).
//! Spawned and supervised by `WorkerAv1Provider`; speaks the length-prefixed protocol on
//! stdin/stdout and exits when stdin closes. It has no command-line interface.
fn main() {
    std::process::exit(hsk_studio_reel_native::worker_main());
}
