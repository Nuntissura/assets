//! Single mapping into an external engine whose device-pixel ratio is one.
use crate::{BridgeError, Result};

/// The caller first validates the global point against the canonical slot clip
/// and subtracts the slot's DIP origin. Engine CSS dimensions equal the slot's
/// physical dimensions, so multiply that local DIP point by host scale once.
/// The resulting integer is the actual WebDriver CSS/pixel coordinate.
pub fn local_dip_to_engine_css(
    local: [f64; 2],
    host_scale: f64,
    engine_size: [u32; 2],
) -> Result<[u32; 2]> {
    if !host_scale.is_finite() || host_scale <= 0.0 || engine_size.contains(&0) {
        return Err(BridgeError::Invalid("input scale/engine extent"));
    }
    let mut css = [0; 2];
    for axis in 0..2 {
        if !local[axis].is_finite() || local[axis] < 0.0 {
            return Err(BridgeError::Invalid("local DIP input"));
        }
        let pixel = local[axis] * host_scale;
        if !pixel.is_finite() || pixel >= f64::from(engine_size[axis]) {
            return Err(BridgeError::Invalid("input outside engine"));
        }
        let rounded = pixel.round();
        if rounded >= f64::from(engine_size[axis]) {
            return Err(BridgeError::Invalid("rounded input outside engine"));
        }
        css[axis] = rounded as u32;
    }
    Ok(css)
}
