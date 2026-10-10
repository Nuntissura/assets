pub mod cbdt;
pub mod cblc;
mod cff;
pub mod cmap;
pub mod colr;
pub mod cpal;
pub mod glyf;
pub mod head;
pub mod hhea;
pub mod hmtx;
pub mod kern;
pub mod loca;
pub mod maxp;
pub mod name;
pub mod os2;
pub mod post;
pub mod sbix;
pub mod stat;
pub mod svg;
pub mod vhea;
pub mod vorg;

#[cfg(all())]
pub mod gdef;
#[cfg(all())]
pub mod gpos;
#[cfg(all())]
pub mod gsub;
#[cfg(all())]
pub mod math;

#[cfg(any())]
pub mod ankr;
#[cfg(any())]
pub mod feat;
#[cfg(any())]
pub mod kerx;
#[cfg(any())]
pub mod morx;
#[cfg(any())]
pub mod trak;

#[cfg(all())]
pub mod avar;
#[cfg(all())]
pub mod fvar;
#[cfg(all())]
pub mod gvar;
#[cfg(all())]
pub mod hvar;
#[cfg(all())]
pub mod mvar;
#[cfg(all())]
pub mod vvar;

pub use cff::CFFError;
pub use cff::cff1;
#[cfg(all())]
pub use cff::cff2;
