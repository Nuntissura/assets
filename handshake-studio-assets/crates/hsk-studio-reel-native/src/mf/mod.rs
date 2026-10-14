//! Media Foundation H.264 provider parts. `nv12` (pure, feature `mf`) converts decoder output; `ffi` (the
//! crate's only `unsafe`) and `worker` exist only in the worker build (feature `mf-worker`, Windows).
pub(crate) mod nv12;

#[cfg(all(feature = "mf-worker", not(windows)))]
compile_error!("feature mf-worker builds the Windows Media Foundation worker and needs a Windows target");

#[cfg(all(feature = "mf-worker", windows))]
mod ffi;
#[cfg(all(feature = "mf-worker", windows))]
pub(crate) mod worker;
