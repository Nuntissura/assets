//! H.264 decoder worker process of `hsk-studio-reel-native` (built only with the `mf-worker` feature, Windows):
//! hosts the OS-provided Media Foundation H.264 decoder. Spawned and supervised by `WorkerMfProvider`; speaks
//! the length-prefixed protocol on stdin/stdout and exits when stdin closes. It has no command-line interface.
fn main() {
    std::process::exit(hsk_studio_reel_native::mf_worker_main());
}
