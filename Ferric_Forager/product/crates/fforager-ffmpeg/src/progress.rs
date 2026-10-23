//! Incremental bounded parser for `FFmpeg -progress pipe:1` records.

/// Aggregate bounded parser result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressSummary {
    pub record_count: u64,
    pub total_bytes: u64,
    pub parser_steps: u64,
    pub saw_terminal: bool,
    /// Exact, correctness-complete bytes read from `-progress pipe:1`.
    ///
    /// This transcript is never truncated. The parser rejects the execution
    /// if the governed evidence allocation cannot retain every byte, so an
    /// independent consumer can replay the grammar and counters.
    pub raw_transcript: Vec<u8>,
}

/// Progress parser rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgressError {
    TotalBytesExceeded,
    RecordCountExceeded,
    RecordBytesExceeded,
    FieldBytesExceeded,
    ParserWorkExceeded,
    EvidenceBytesExceeded,
    MalformedLine,
    DuplicateField,
    ReorderedAfterTerminal,
    TruncatedRecord,
    MissingTerminal,
}

/// Exact limits for incremental progress parsing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_field_names,
    reason = "the public limit names mirror the versioned FFmpeg contract and distinguish every independent ceiling"
)]
pub struct ProgressLimits {
    pub max_records: u64,
    pub max_total_bytes: u64,
    pub max_record_bytes: u64,
    pub max_field_bytes: u64,
    pub max_parser_steps: u64,
}

/// Stateful fixed-bound parser; callers feed arbitrary pipe-sized chunks.
#[derive(Debug)]
pub struct ProgressParser {
    limits: ProgressLimits,
    summary: ProgressSummary,
    line: Vec<u8>,
    record: Vec<u8>,
    record_bytes: u64,
    transcript_capacity: usize,
}

impl ProgressParser {
    #[must_use]
    pub fn new(limits: ProgressLimits, transcript_capacity: usize) -> Self {
        Self {
            limits,
            summary: ProgressSummary {
                raw_transcript: Vec::with_capacity(transcript_capacity),
                ..ProgressSummary::default()
            },
            line: Vec::with_capacity(
                usize::try_from(limits.max_record_bytes).unwrap_or(usize::MAX),
            ),
            record: Vec::with_capacity(
                usize::try_from(limits.max_record_bytes).unwrap_or(usize::MAX),
            ),
            record_bytes: 0,
            transcript_capacity,
        }
    }

    /// Feed bytes while enforcing total, record, field, and parser-work limits.
    ///
    /// # Errors
    ///
    /// Returns the first bound or grammar failure and never drops required data.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ProgressError> {
        let transcript_bytes = self
            .summary
            .raw_transcript
            .len()
            .checked_add(bytes.len())
            .ok_or(ProgressError::EvidenceBytesExceeded)?;
        if transcript_bytes > self.transcript_capacity {
            return Err(ProgressError::EvidenceBytesExceeded);
        }
        self.summary.total_bytes = self
            .summary
            .total_bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| ProgressError::TotalBytesExceeded)?)
            .ok_or(ProgressError::TotalBytesExceeded)?;
        if self.summary.total_bytes > self.limits.max_total_bytes {
            return Err(ProgressError::TotalBytesExceeded);
        }
        self.summary.raw_transcript.extend_from_slice(bytes);
        for byte in bytes {
            self.summary.parser_steps = self
                .summary
                .parser_steps
                .checked_add(1)
                .ok_or(ProgressError::ParserWorkExceeded)?;
            if self.summary.parser_steps > self.limits.max_parser_steps {
                return Err(ProgressError::ParserWorkExceeded);
            }
            if *byte == b'\n' {
                let mut line = std::mem::take(&mut self.line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                self.parse_line(&line)?;
            } else {
                self.line.push(*byte);
                if u64::try_from(self.line.len()).unwrap_or(u64::MAX) > self.limits.max_record_bytes
                {
                    return Err(ProgressError::RecordBytesExceeded);
                }
            }
        }
        Ok(())
    }

    fn parse_line(&mut self, line: &[u8]) -> Result<(), ProgressError> {
        if self.summary.saw_terminal {
            return Err(ProgressError::ReorderedAfterTerminal);
        }
        self.record_bytes = self
            .record_bytes
            .checked_add(u64::try_from(line.len() + 1).unwrap_or(u64::MAX))
            .ok_or(ProgressError::RecordBytesExceeded)?;
        if self.record_bytes > self.limits.max_record_bytes {
            return Err(ProgressError::RecordBytesExceeded);
        }
        let text = std::str::from_utf8(line).map_err(|_| ProgressError::MalformedLine)?;
        let (key, value) = text.split_once('=').ok_or(ProgressError::MalformedLine)?;
        if key.is_empty()
            || u64::try_from(key.len()).unwrap_or(u64::MAX) > self.limits.max_field_bytes
            || u64::try_from(value.len()).unwrap_or(u64::MAX) > self.limits.max_field_bytes
        {
            return Err(ProgressError::FieldBytesExceeded);
        }
        if !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(ProgressError::MalformedLine);
        }
        // Duplicate detection scans prior record bytes. Charge a conservative
        // two passes plus the candidate comparison so parser-work is a real
        // CPU bound rather than merely an input-byte counter.
        let duplicate_scan_steps = self
            .record
            .len()
            .checked_mul(2)
            .and_then(|steps| steps.checked_add(key.len()))
            .and_then(|steps| u64::try_from(steps).ok())
            .ok_or(ProgressError::ParserWorkExceeded)?;
        self.summary.parser_steps = self
            .summary
            .parser_steps
            .checked_add(duplicate_scan_steps)
            .ok_or(ProgressError::ParserWorkExceeded)?;
        if self.summary.parser_steps > self.limits.max_parser_steps {
            return Err(ProgressError::ParserWorkExceeded);
        }
        if record_contains_key(&self.record, key.as_bytes()) {
            return Err(ProgressError::DuplicateField);
        }
        if key == "progress" {
            let terminal = match value {
                "continue" => false,
                "end" => true,
                _ => return Err(ProgressError::MalformedLine),
            };
            if self.summary.record_count >= self.limits.max_records {
                return Err(ProgressError::RecordCountExceeded);
            }
            self.summary.record_count = self
                .summary
                .record_count
                .checked_add(1)
                .ok_or(ProgressError::RecordCountExceeded)?;
            self.record.clear();
            self.record_bytes = 0;
            self.summary.saw_terminal = terminal;
        } else {
            self.record.extend_from_slice(line);
            self.record.push(b'\n');
        }
        Ok(())
    }

    /// Finish the stream and require an exact terminal record.
    ///
    /// # Errors
    ///
    /// Rejects a partial line, partial record, or missing `progress=end`.
    pub fn finish(self) -> Result<ProgressSummary, ProgressError> {
        if !self.line.is_empty() || !self.record.is_empty() {
            return Err(ProgressError::TruncatedRecord);
        }
        if !self.summary.saw_terminal {
            return Err(ProgressError::MissingTerminal);
        }
        Ok(self.summary)
    }
}

/// Independently replay an exact progress transcript under the declared
/// grammar and work limits.
///
/// # Errors
///
/// Returns the same typed grammar or bound failure as live parsing.
pub fn replay_progress_transcript(
    transcript: &[u8],
    limits: ProgressLimits,
) -> Result<ProgressSummary, ProgressError> {
    let mut parser = ProgressParser::new(limits, transcript.len());
    parser.feed(transcript)?;
    parser.finish()
}

fn record_contains_key(record: &[u8], candidate: &[u8]) -> bool {
    record.split(|byte| *byte == b'\n').any(|line| {
        line.iter()
            .position(|byte| *byte == b'=')
            .is_some_and(|separator| &line[..separator] == candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> ProgressLimits {
        ProgressLimits {
            max_records: 4,
            max_total_bytes: 256,
            max_record_bytes: 128,
            max_field_bytes: 64,
            max_parser_steps: 512,
        }
    }

    #[test]
    fn chunked_records_require_terminal_end() {
        let mut parser = ProgressParser::new(limits(), 256);
        parser.feed(b"frame=1\npro").expect("first chunk");
        parser
            .feed(b"gress=continue\nframe=2\nprogress=end\n")
            .expect("second chunk");
        let summary = parser.finish().expect("complete stream");
        assert_eq!(summary.record_count, 2);
        assert!(summary.saw_terminal);
        assert_eq!(
            summary.raw_transcript,
            b"frame=1\nprogress=continue\nframe=2\nprogress=end\n"
        );
    }

    #[test]
    fn reordered_data_after_terminal_is_rejected() {
        let mut parser = ProgressParser::new(limits(), 256);
        assert_eq!(
            parser.feed(b"progress=end\nframe=3\n"),
            Err(ProgressError::ReorderedAfterTerminal)
        );
    }

    #[test]
    fn truncation_is_not_silently_accepted() {
        let mut parser = ProgressParser::new(limits(), 256);
        parser.feed(b"frame=1\nprogress=en").expect("feed");
        assert_eq!(parser.finish(), Err(ProgressError::TruncatedRecord));
    }

    #[test]
    fn duplicate_scans_are_charged_to_parser_work() {
        let input = (0..256)
            .map(|index| format!("k{index}=v\n"))
            .chain(std::iter::once("progress=end\n".to_owned()))
            .collect::<String>();
        let exact_input_bytes = u64::try_from(input.len()).expect("input length");
        let mut parser = ProgressParser::new(
            ProgressLimits {
                max_records: 1,
                max_total_bytes: exact_input_bytes,
                max_record_bytes: exact_input_bytes,
                max_field_bytes: 32,
                max_parser_steps: exact_input_bytes,
            },
            usize::try_from(exact_input_bytes).expect("evidence capacity"),
        );
        assert_eq!(
            parser.feed(input.as_bytes()),
            Err(ProgressError::ParserWorkExceeded)
        );
    }

    #[test]
    fn correctness_transcript_never_truncates_to_fit_evidence_allocation() {
        let mut parser = ProgressParser::new(limits(), 12);
        assert_eq!(
            parser.feed(b"progress=end\n"),
            Err(ProgressError::EvidenceBytesExceeded)
        );
    }

    #[test]
    fn retained_transcript_replays_exact_counters_and_terminal_grammar() {
        let transcript = b"frame=1\nprogress=continue\nframe=2\nprogress=end\n";
        let replayed = replay_progress_transcript(transcript, limits()).expect("replay");
        assert_eq!(replayed.total_bytes, transcript.len() as u64);
        assert_eq!(replayed.record_count, 2);
        assert!(replayed.saw_terminal);
        assert_eq!(replayed.raw_transcript, transcript);

        let mutated = b"frame=1\nprogress=continue\nframe=2\nprogress=continue\n";
        assert_eq!(
            replay_progress_transcript(mutated, limits()),
            Err(ProgressError::MissingTerminal)
        );
    }
}
