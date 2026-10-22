//! Standalone CLAP test fixture for `hsk-studio-score-plugin` (NOT a Studio workspace member): a
//! stereo gain plugin with a FIXED gain of 0.5 and no parameters, written with `clack-plugin` 0.2.0.
//! The expected output (`input * 0.5`, exact in f32) is defined here, independently of the host code
//! that is being proven. Build it into its own target directory (see `tests/clap_fixture.rs`).
use clack_plugin::prelude::*;

/// CLAP plugin id the test expects from the probe.
pub const FIXTURE_ID: &str = "hsk.score-plugin.clap-gain-fixture";
const GAIN: f32 = 0.5;

pub struct GainFixture;

impl Plugin for GainFixture {
    type AudioProcessor<'a> = GainFixtureProcessor;
    type Shared<'a> = ();
    type MainThread<'a> = ();
}

impl DefaultPluginFactory for GainFixture {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(FIXTURE_ID, "HSK Score Plugin CLAP Gain Fixture 0.5")
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Self::Shared<'_>, PluginError> {
        Ok(())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a Self::Shared<'a>,
    ) -> Result<Self::MainThread<'a>, PluginError> {
        Ok(())
    }
}

pub struct GainFixtureProcessor;

impl<'a> PluginAudioProcessor<'a, (), ()> for GainFixtureProcessor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &(),
        _shared: &'a (),
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            for channel_pair in channel_pairs {
                match channel_pair {
                    ChannelPair::InputOnly(_) => {}
                    ChannelPair::OutputOnly(buf) => buf.fill(0.0),
                    ChannelPair::InputOutput(input, output) => {
                        for (input, output) in input.iter().zip(output) {
                            *output = *input * GAIN;
                        }
                    }
                    ChannelPair::InPlace(buf) => {
                        for sample in buf {
                            *sample *= GAIN;
                        }
                    }
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

clack_export_entry!(SinglePluginEntry<GainFixture>);
