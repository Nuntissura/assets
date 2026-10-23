//! QuickJS worker process of `hsk-studio-motion-js` (built only with the `quickjs` feature).
//! Spawned and supervised by the host provider; speaks the length-prefixed protocol on
//! stdin/stdout and exits when stdin closes. It has no command-line interface.
fn main() {
    std::process::exit(hsk_studio_motion_js::worker_main());
}
