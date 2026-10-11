use crate::error::DeviceError;
use hsk_studio_accord::{ActorContext, DomainId};
use hsk_studio_score::{ChannelLayout, MAX_BLOCK_FRAMES, ticks_per_sample};

/// Bytes per ring sample (`f32`, the only sample format of this crate).
pub const SAMPLE_BYTES: usize = 4;
/// Smallest ring (in blocks) that still double-buffers.
pub const MIN_RING_BLOCKS: u32 = 2;

/// Authored admission bounds (not spec values). All checked before any resource exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Upper bound on the preallocated ring of one stream.
    pub max_ring_bytes: usize,
    /// Upper bound on frames in one block (also capped by Score `MAX_BLOCK_FRAMES`).
    pub max_block_frames: u32,
    /// Upper bound on blocks in one ring.
    pub max_ring_blocks: u32,
    /// Streams that may be open at once through one provider admission.
    pub max_open_streams: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_ring_bytes: 4 * 1024 * 1024,
            max_block_frames: 16_384,
            max_ring_blocks: 256,
            max_open_streams: 4,
        }
    }
}

/// Stream shape negotiated off the realtime path. Samples are interleaved `f32`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamConfig {
    pub sample_rate_hz: u32,
    pub layout: ChannelLayout,
    pub block_frames: u32,
    pub ring_blocks: u32,
}

/// Validated sizes derived from a [`StreamConfig`]; computed with checked arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingGeometry {
    pub channels: usize,
    pub block_frames: usize,
    pub block_samples: usize,
    pub capacity_samples: usize,
    pub ring_bytes: usize,
}

impl StreamConfig {
    /// Validation order: rate, block frames, ring blocks, ring bytes. Allocates nothing.
    pub fn validate(&self, limits: &Limits) -> Result<RingGeometry, DeviceError> {
        ticks_per_sample(self.sample_rate_hz).map_err(|_| DeviceError::InvalidRate)?;
        let frame_cap = limits.max_block_frames.min(MAX_BLOCK_FRAMES as u32);
        if self.block_frames == 0 || self.block_frames > frame_cap {
            return Err(DeviceError::InvalidBlockFrames);
        }
        if self.ring_blocks < MIN_RING_BLOCKS || self.ring_blocks > limits.max_ring_blocks {
            return Err(DeviceError::InvalidRingBlocks);
        }
        let channels = self.layout.channels();
        let block_frames = self.block_frames as usize;
        let block_samples = block_frames
            .checked_mul(channels)
            .ok_or(DeviceError::RingTooLarge)?;
        let capacity_samples = block_samples
            .checked_mul(self.ring_blocks as usize)
            .ok_or(DeviceError::RingTooLarge)?;
        // Ring positions run in [0, 2 * capacity); that must not overflow either.
        capacity_samples
            .checked_mul(2)
            .ok_or(DeviceError::RingTooLarge)?;
        let ring_bytes = capacity_samples
            .checked_mul(SAMPLE_BYTES)
            .ok_or(DeviceError::RingTooLarge)?;
        if ring_bytes > limits.max_ring_bytes {
            return Err(DeviceError::RingTooLarge);
        }
        Ok(RingGeometry {
            channels,
            block_frames,
            block_samples,
            capacity_samples,
            ring_bytes,
        })
    }
}

/// Opaque device identifier (1-128 chars, no control characters).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceId(String);

impl DeviceId {
    pub fn parse(value: &str) -> Result<Self, DeviceError> {
        let len = value.chars().count();
        if len == 0 || len > 128 || value.chars().any(char::is_control) {
            return Err(DeviceError::InvalidDeviceId);
        }
        Ok(Self(value.to_owned()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Transfer direction of a stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Output,
    Input,
}

/// One enumerated device and its declared capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub max_output_channels: u16,
    pub max_input_channels: u16,
    pub min_sample_rate_hz: u32,
    pub max_sample_rate_hz: u32,
}

/// Explicit caller-supplied authority to touch devices. There is no ambient device access: every
/// provider call takes `Option<&DeviceGrant>` and `None` is `Unauthorized`. The grant is a bounded
/// transport token, not proof of a host grant (same stance as accord `ActorContext`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceGrant {
    actor: ActorContext,
    resource: DomainId,
    output: bool,
    input: bool,
    device: Option<DeviceId>,
}

impl DeviceGrant {
    /// `device = None` covers every device the provider knows; `Some` pins the grant to one.
    pub fn new(
        actor: ActorContext,
        resource: DomainId,
        directions: &[Direction],
        device: Option<DeviceId>,
    ) -> Self {
        Self {
            actor,
            resource,
            output: directions.contains(&Direction::Output),
            input: directions.contains(&Direction::Input),
            device,
        }
    }
    pub fn actor(&self) -> &ActorContext {
        &self.actor
    }
    pub fn resource(&self) -> &DomainId {
        &self.resource
    }
    /// True when the grant covers this direction on this device.
    pub fn permits(&self, direction: Direction, device: &DeviceId) -> bool {
        let direction_ok = match direction {
            Direction::Output => self.output,
            Direction::Input => self.input,
        };
        direction_ok && self.device.as_ref().is_none_or(|d| d == device)
    }
    /// Listing devices needs a grant of any kind.
    pub fn permits_enumerate(&self) -> bool {
        self.output || self.input
    }
}

/// Grant gate shared by every provider: no grant, or a grant that does not cover this direction
/// and device, is `Unauthorized`. Runs before anything is created.
pub(crate) fn authorize(
    grant: Option<&DeviceGrant>,
    direction: Direction,
    device: &DeviceId,
) -> Result<(), DeviceError> {
    match grant {
        Some(g) if g.permits(direction, device) => Ok(()),
        _ => Err(DeviceError::Unauthorized),
    }
}
