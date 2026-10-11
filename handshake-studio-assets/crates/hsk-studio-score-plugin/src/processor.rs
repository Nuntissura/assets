//! Adapter that lets a score `Engine` drive a hosted plugin: the engine admits the block, this adapter
//! runs it through the isolated instance, and the engine alone publishes it (or discards it on any
//! failure, so a crashed or timed-out plugin never leaves partial audio).
use crate::error::PluginError;
use crate::types::{PluginInstance, ProcessReceipt};
use hsk_studio_accord::CancellationToken;
use hsk_studio_score::{ChannelLayout, InputBlock, OutputBlock, Processor, ScoreError};
use std::time::Duration;

/// `Processor` over `&mut dyn PluginInstance`.
///
/// The engine's error vocabulary is closed, so the typed failure is kept here: `take_error` returns the
/// exact [`PluginError`] of the last failed block. Mapping to `ScoreError`: `Canceled` -> `Canceled`,
/// `BudgetExceeded` -> `CapacityExceeded`, `InvalidBlock(e)` -> `e`, `NonFiniteOutput` -> `NotFinite`,
/// every other hosted-process fault -> `Busy` (the provider is unavailable; score's `outcome_of` reports
/// it as `Failure(Unavailable)`).
///
/// Known limit: when the engine discards a block (`reset`), the foreign plugin's internal history has
/// already advanced past it; a plugin with memory is not rewound.
pub struct PluginProcessor<'a> {
    instance: &'a mut dyn PluginInstance,
    deadline: Duration,
    last_receipt: Option<ProcessReceipt>,
    last_error: Option<PluginError>,
}

impl<'a> PluginProcessor<'a> {
    pub fn new(instance: &'a mut dyn PluginInstance, deadline: Duration) -> Self {
        Self {
            instance,
            deadline,
            last_receipt: None,
            last_error: None,
        }
    }

    /// Receipt of the last block that completed in the instance.
    pub fn last_receipt(&self) -> Option<&ProcessReceipt> {
        self.last_receipt.as_ref()
    }

    /// The typed failure of the last block, once.
    pub fn take_error(&mut self) -> Option<PluginError> {
        self.last_error.take()
    }
}

const fn score_error(error: PluginError) -> ScoreError {
    match error {
        PluginError::Canceled => ScoreError::Canceled,
        PluginError::BudgetExceeded => ScoreError::CapacityExceeded,
        PluginError::InvalidBlock(inner) => inner,
        PluginError::NonFiniteOutput => ScoreError::NotFinite,
        _ => ScoreError::Busy,
    }
}

impl Processor for PluginProcessor<'_> {
    fn latency_samples(&self) -> u32 {
        self.instance.latency_samples()
    }

    fn accepts(&self, input: &ChannelLayout, output: &ChannelLayout) -> bool {
        input == self.instance.layout() && output == self.instance.layout()
    }

    fn required_sample_rate_hz(&self) -> Option<u32> {
        Some(self.instance.sample_rate_hz())
    }

    fn identity(&self) -> String {
        format!(
            "plugin:{}:instance={}:state={}",
            self.instance.provider_identity(),
            self.instance.instance_id(),
            self.instance.state_version()
        )
    }

    fn max_frames(&self) -> Option<usize> {
        Some(self.instance.max_block_frames())
    }

    fn process(
        &mut self,
        input: &InputBlock<'_>,
        out: &mut OutputBlock,
        cancel: &CancellationToken,
    ) -> Result<(), ScoreError> {
        match self.instance.process(input, out, cancel, self.deadline) {
            Ok(receipt) => {
                self.last_receipt = Some(receipt);
                Ok(())
            }
            Err(error) => {
                self.last_error = Some(error);
                Err(score_error(error))
            }
        }
    }
}
