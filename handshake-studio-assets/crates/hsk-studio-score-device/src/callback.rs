//! The two realtime callback bodies, shared by every backend. They touch only preallocated ring
//! storage and relaxed atomic counters: no allocation, no waiting, no logging, no formatting.
//! Backends call these from their device callback (or, for the software device, from the
//! clocked thread); everything else - open, close, negotiation - happens off this path.
use crate::ring::{Consumer, Producer};

/// Output callback body: copy the frames the ring holds into `out`, then fill the remainder with
/// digital silence (the defined safe underrun output) and count the shortfall.
///
/// `out` is interleaved `f32`; a trailing partial frame (never produced by a real device) is
/// zero-filled as well.
pub fn render_callback(consumer: &mut Consumer, out: &mut [f32]) {
    let copied = consumer.pop_into(out);
    let counters = consumer.counters();
    counters.add_callback();
    if copied < out.len() {
        for sample in &mut out[copied..] {
            *sample = 0.0;
        }
        let silent_frames = (out.len() - copied) / consumer.channels();
        if silent_frames > 0 {
            counters.add_underrun(silent_frames as u64);
        }
    }
}

/// Input callback body: push every whole frame of `input` that fits; the rest is lost to the
/// stream and counted as one overrun plus one discontinuity (see `Producer::push_frames`).
/// Returns the frames accepted.
pub fn capture_callback(producer: &mut Producer, input: &[f32]) -> u64 {
    producer.counters().add_callback();
    producer.push_frames(input)
}
