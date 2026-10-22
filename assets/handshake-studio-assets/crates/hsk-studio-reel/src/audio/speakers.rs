//! Loudspeaker positions of decoded channels (lane SMB-AUDIO-IMPORT-2). Each format's own channel-order
//! rule, as delivered by reel's decoders (no reordering happens in reel):
//!
//! * FLAC (native and Ogg FLAC), RFC 9639 section 9.1.3: 1 mono; 2 L R; 3 L R C; 4 FL FR BL BR;
//!   5 FL FR FC BL BR; 6 FL FR FC LFE BL BR; 7 FL FR FC LFE BC SL SR; 8 FL FR FC LFE BL BR SL SR.
//! * Vorbis and Opus mapping family 1, Vorbis I section 4.3.9: 3 L C R; 4 FL FR RL RR; 5 FL C FR RL RR;
//!   6 FL C FR RL RR LFE; 7 FL C FR SL SR RC LFE; 8 FL C FR SL SR RL RR LFE.
//! * AAC-LC element order for channel configurations 1-6 (ISO/IEC 14496-3 channel configuration
//!   table): 3 C L R; 4 C L R Cs; 5 C L R Ls Rs; 6 C L R Ls Rs LFE. Configuration 7 (8 channels) is
//!   read as 7.1 or as 7.1 wide by different decoders, so no order is claimed (`None`).
//! * WAV: the WAVE_FORMAT_EXTENSIBLE channel mask (bits 0-10 below, in bit order) when present and
//!   its population equals the channel count; without a mask only 1 and 2 channels have an order.
//! * AIFF / AIFF-C and MPEG audio: 1 and 2 channels only.
//!
//! Vorbis "rear", FLAC "back/surround" and the AAC surround pair are the 3/2 surround pair and map to
//! `BackLeft` / `BackRight`; only explicit side channels (FLAC 7/8, Vorbis 7/8, WAV mask bits 9/10) are
//! `SideLeft` / `SideRight`.

/// One loudspeaker position (WAVE_FORMAT_EXTENSIBLE bits 0-10 order), or a channel to drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Speaker {
    FrontLeft,
    FrontRight,
    FrontCenter,
    LowFrequency,
    BackLeft,
    BackRight,
    FrontLeftOfCenter,
    FrontRightOfCenter,
    BackCenter,
    SideLeft,
    SideRight,
    /// Not reproduced (a caller channel map can drop a channel with this).
    Unused,
}

impl Speaker {
    /// WAVE_FORMAT_EXTENSIBLE mask bits 0..=10 in bit order.
    pub const MASK_ORDER: [Speaker; 11] = [
        Speaker::FrontLeft,
        Speaker::FrontRight,
        Speaker::FrontCenter,
        Speaker::LowFrequency,
        Speaker::BackLeft,
        Speaker::BackRight,
        Speaker::FrontLeftOfCenter,
        Speaker::FrontRightOfCenter,
        Speaker::BackCenter,
        Speaker::SideLeft,
        Speaker::SideRight,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FrontLeft => "FL",
            Self::FrontRight => "FR",
            Self::FrontCenter => "FC",
            Self::LowFrequency => "LFE",
            Self::BackLeft => "BL",
            Self::BackRight => "BR",
            Self::FrontLeftOfCenter => "FLC",
            Self::FrontRightOfCenter => "FRC",
            Self::BackCenter => "BC",
            Self::SideLeft => "SL",
            Self::SideRight => "SR",
            Self::Unused => "unused",
        }
    }
}

/// Channel-order rule of a decoded stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelOrder {
    Flac,
    Vorbis,
    Aac,
    /// WAV with an optional WAVE_FORMAT_EXTENSIBLE channel mask.
    Wav(Option<u32>),
    /// AIFF, MPEG audio: mono or stereo only.
    MonoStereo,
}

use Speaker::*;

fn mono_stereo(channels: u16) -> Option<Vec<Speaker>> {
    match channels {
        1 => Some(vec![FrontCenter]),
        2 => Some(vec![FrontLeft, FrontRight]),
        _ => None,
    }
}

/// Speakers of `channels` channels under `order`; `None` when the rule defines no order for the count.
pub(crate) fn layout_for(order: ChannelOrder, channels: u16) -> Option<Vec<Speaker>> {
    let table: &[Speaker] = match (order, channels) {
        (_, 1 | 2) if !matches!(order, ChannelOrder::Wav(Some(_))) => return mono_stereo(channels),
        (ChannelOrder::Flac, 3) => &[FrontLeft, FrontRight, FrontCenter],
        (ChannelOrder::Flac, 4) => &[FrontLeft, FrontRight, BackLeft, BackRight],
        (ChannelOrder::Flac, 5) => &[FrontLeft, FrontRight, FrontCenter, BackLeft, BackRight],
        (ChannelOrder::Flac, 6) => &[FrontLeft, FrontRight, FrontCenter, LowFrequency, BackLeft, BackRight],
        (ChannelOrder::Flac, 7) => &[FrontLeft, FrontRight, FrontCenter, LowFrequency, BackCenter, SideLeft, SideRight],
        (ChannelOrder::Flac, 8) => &[
            FrontLeft, FrontRight, FrontCenter, LowFrequency, BackLeft, BackRight, SideLeft, SideRight,
        ],
        (ChannelOrder::Vorbis, 3) => &[FrontLeft, FrontCenter, FrontRight],
        (ChannelOrder::Vorbis, 4) => &[FrontLeft, FrontRight, BackLeft, BackRight],
        (ChannelOrder::Vorbis, 5) => &[FrontLeft, FrontCenter, FrontRight, BackLeft, BackRight],
        (ChannelOrder::Vorbis, 6) => &[FrontLeft, FrontCenter, FrontRight, BackLeft, BackRight, LowFrequency],
        (ChannelOrder::Vorbis, 7) => &[FrontLeft, FrontCenter, FrontRight, SideLeft, SideRight, BackCenter, LowFrequency],
        (ChannelOrder::Vorbis, 8) => &[
            FrontLeft, FrontCenter, FrontRight, SideLeft, SideRight, BackLeft, BackRight, LowFrequency,
        ],
        (ChannelOrder::Aac, 3) => &[FrontCenter, FrontLeft, FrontRight],
        (ChannelOrder::Aac, 4) => &[FrontCenter, FrontLeft, FrontRight, BackCenter],
        (ChannelOrder::Aac, 5) => &[FrontCenter, FrontLeft, FrontRight, BackLeft, BackRight],
        (ChannelOrder::Aac, 6) => &[FrontCenter, FrontLeft, FrontRight, BackLeft, BackRight, LowFrequency],
        (ChannelOrder::Wav(Some(mask)), n) => {
            if mask & !0x7FF != 0 || mask.count_ones() != u32::from(n) {
                return None;
            }
            return Some(
                Speaker::MASK_ORDER
                    .iter()
                    .enumerate()
                    .filter(|(bit, _)| mask & (1u32 << bit) != 0)
                    .map(|(_, s)| *s)
                    .collect(),
            );
        }
        _ => return None,
    };
    Some(table.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_match_their_tables_and_masks() {
        assert_eq!(
            layout_for(ChannelOrder::Vorbis, 6).unwrap(),
            [FrontLeft, FrontCenter, FrontRight, BackLeft, BackRight, LowFrequency]
        );
        assert_eq!(layout_for(ChannelOrder::Aac, 8), None);
        assert_eq!(layout_for(ChannelOrder::Wav(None), 6), None);
        assert_eq!(layout_for(ChannelOrder::Wav(None), 2).unwrap(), [FrontLeft, FrontRight]);
        // 5.1 mask 0x3F, 5.1(side) 0x60F.
        assert_eq!(
            layout_for(ChannelOrder::Wav(Some(0x60F)), 6).unwrap(),
            [FrontLeft, FrontRight, FrontCenter, LowFrequency, SideLeft, SideRight]
        );
        assert_eq!(layout_for(ChannelOrder::Wav(Some(0x3F)), 5), None, "mask population != channels");
        assert_eq!(layout_for(ChannelOrder::Wav(Some(0x1_0003)), 3), None, "top speakers unsupported");
        for n in 1..=8 {
            assert_eq!(layout_for(ChannelOrder::Flac, n).map(|v| v.len()), Some(usize::from(n)));
        }
    }
}
