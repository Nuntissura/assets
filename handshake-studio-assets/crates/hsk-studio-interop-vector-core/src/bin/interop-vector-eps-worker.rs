//! Isolated EPS-to-PDF worker of `hsk-studio-interop-vector-core` (feature `eps-worker`; lane
//! SMB-DXF-EPS-IMPORT). Spawned and supervised by `WorkerEpsConverter`; speaks the length-prefixed
//! protocol on stdin/stdout and exits when stdin closes. It has no command-line interface.
#![forbid(unsafe_code)]
fn main() {
    std::process::exit(hsk_studio_interop_vector_core::eps_worker_main());
}
