//! Signature-help model, LSP conversion and label spans.

use std::ops::Range;

/// Where a [`SignatureHelpState`] came from. The renderer does not branch on this, but the panel uses
/// it to decide whether a comma should RE-REQUEST from the LSP server (it owns the active parameter) or
/// RECOMPUTE locally (the code-nav fallback computes the active parameter from commas).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureSource {
    /// The signature came from an attached language server's `textDocument/signatureHelp`.
    Lsp,
    /// The signature was synthesized from the Handshake backend code-nav symbol (the fallback).
    CodeNavFallback,
}

/// One parameter of a signature: its display text plus the byte range it occupies WITHIN the signature
/// label, so the renderer can emphasize exactly that run. `range_in_label` is resolved from the LSP
/// `LabelOffsets([start, end])` form when present, otherwise by a substring search of the simple label
/// within the signature label (left-to-right, consuming earlier matched ranges so a repeated parameter
/// name maps to a DISTINCT span — RISK-005 / MC-005). `None` when the parameter text could not be
/// located in the label (the renderer then emphasizes nothing for it rather than guessing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamSpan {
    /// The parameter's display text (e.g. `a: i32`).
    pub label: String,
    /// The byte range this parameter occupies in the signature label, when locatable.
    pub range_in_label: Option<Range<usize>>,
}

/// One callable signature: the full label (e.g. `fn add(a: i32, b: i32) -> i32`), its parameters (with
/// their spans in the label), and an optional documentation string (the first line is shown).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    /// The full signature label shown in the popup.
    pub label: String,
    /// The parameters, in order, with their spans into `label`.
    pub parameters: Vec<ParamSpan>,
    /// Optional documentation (the popup shows the first non-empty line).
    pub documentation: Option<String>,
}

impl SignatureInfo {
    /// Build a signature from a label + an ORDERED list of parameter display strings, resolving each
    /// parameter's span by a left-to-right substring search of the label (the `ParameterLabel::Simple`
    /// path and the code-nav fallback path). Earlier matches are consumed so a repeated parameter name
    /// (`fn f(x: i32, x: i32)`) maps each occurrence to a DISTINCT span (RISK-005 / MC-005).
    pub fn from_label_and_param_strings(label: impl Into<String>, params: Vec<String>) -> Self {
        let label = label.into();
        let mut parameters = Vec::with_capacity(params.len());
        // `search_from` advances past each matched run so repeated names resolve to distinct spans.
        let mut search_from = 0usize;
        for p in params {
            let range = substring_range_from(&label, &p, search_from);
            if let Some(r) = &range {
                search_from = r.end;
            }
            parameters.push(ParamSpan {
                label: p,
                range_in_label: range,
            });
        }
        Self {
            label,
            parameters,
            documentation: None,
        }
    }
}

/// The live signature-help popup state, owned by the editor pane. Present only while the popup is open.
/// `anchor_byte` is the call's open-paren byte offset — the KEY that distinguishes call sites so a
/// comma updates the open popup (same `anchor_byte`) rather than opening a second one (RISK-002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureHelpState {
    /// The available signatures (overloads). At least one when the popup is open.
    pub signatures: Vec<SignatureInfo>,
    /// The active signature index (overload selection; Up/Down cycles it). Clamped before use.
    pub active_signature: usize,
    /// The active parameter index within the active signature (the emphasized run). Clamped before use.
    pub active_parameter: usize,
    /// The call's open-paren byte offset — the call-site key (RISK-002) + the dismissal anchor.
    pub anchor_byte: usize,
    /// Where this state came from (LSP vs the code-nav fallback).
    pub source: SignatureSource,
}

impl SignatureHelpState {
    /// The currently-active signature (clamped), or `None` when there are no signatures.
    pub fn active(&self) -> Option<&SignatureInfo> {
        if self.signatures.is_empty() {
            return None;
        }
        let idx = self.active_signature.min(self.signatures.len() - 1);
        self.signatures.get(idx)
    }

    /// Cycle to the NEXT overload (Down arrow), wrapping. No-op with 0/1 signatures.
    pub fn select_next_signature(&mut self) {
        if self.signatures.len() > 1 {
            self.active_signature = (self.active_signature + 1) % self.signatures.len();
            // A new overload may have fewer parameters; clamp the active parameter.
            self.clamp_active_parameter();
        }
    }

    /// Cycle to the PREVIOUS overload (Up arrow), wrapping. No-op with 0/1 signatures.
    pub fn select_prev_signature(&mut self) {
        let n = self.signatures.len();
        if n > 1 {
            self.active_signature = (self.active_signature + n - 1) % n;
            self.clamp_active_parameter();
        }
    }

    /// Clamp `active_parameter` to the active signature's parameter count (so an overload switch / a
    /// recomputed comma count past the last parameter never indexes out of range).
    pub fn clamp_active_parameter(&mut self) {
        let count = self.active().map(|s| s.parameters.len()).unwrap_or(0);
        if count == 0 {
            self.active_parameter = 0;
        } else if self.active_parameter >= count {
            self.active_parameter = count - 1;
        }
    }

    /// Convert an `lsp_types::SignatureHelp` into the transport-agnostic state, resolving each
    /// parameter's span (BOTH `ParameterLabel` forms — MC-005) and reading the active indices from the
    /// response (`active_signature` / `active_parameter`, with the per-signature `active_parameter`
    /// override the LSP 3.16 spec allows). `anchor_byte` is the call's open-paren offset (the panel
    /// passes it; the LSP response does not carry it). Returns `None` when the server sent no signatures
    /// (the caller then falls back to the code-nav path — AC-003). NEVER panics on a malformed field
    /// (AC-008): missing/out-of-range indices clamp to 0.
    pub fn from_lsp(help: &lsp_types::SignatureHelp, anchor_byte: usize) -> Option<Self> {
        if help.signatures.is_empty() {
            return None;
        }
        let active_signature = help.active_signature.unwrap_or(0) as usize;
        let signatures: Vec<SignatureInfo> = help
            .signatures
            .iter()
            .map(signature_info_from_lsp)
            .collect();
        // Per LSP 3.16, a signature's own `active_parameter` (if present) takes precedence over the
        // top-level one for THAT signature; we read the active signature's override when set.
        let sig_idx = active_signature.min(signatures.len() - 1);
        let active_parameter = help
            .signatures
            .get(sig_idx)
            .and_then(|s| s.active_parameter)
            .or(help.active_parameter)
            .unwrap_or(0) as usize;
        let mut state = Self {
            signatures,
            active_signature,
            active_parameter,
            anchor_byte,
            source: SignatureSource::Lsp,
        };
        state.clamp_active_parameter();
        // Clamp the signature index too (a server could send active_signature past the list).
        if state.active_signature >= state.signatures.len() {
            state.active_signature = 0;
        }
        Some(state)
    }



}

/// Convert one `lsp_types::SignatureInformation` to a [`SignatureInfo`], resolving each parameter's span
/// from BOTH `ParameterLabel` forms (MC-005). A `LabelOffsets([start, end])` is taken directly (clamped
/// to the label length so a server's bad offset never panics — AC-008); a `Simple(name)` is resolved by
/// a left-to-right substring search consuming earlier matches (RISK-005). Documentation flattens to its
/// string form.
fn signature_info_from_lsp(sig: &lsp_types::SignatureInformation) -> SignatureInfo {
    let label = sig.label.clone();
    let mut parameters = Vec::new();
    let mut search_from = 0usize; // advances for the Simple-form left-to-right search.
    if let Some(params) = &sig.parameters {
        for p in params {
            let (text, range) = match &p.label {
                lsp_types::ParameterLabel::LabelOffsets([start, end]) => {
                    let s = (*start as usize).min(label.len());
                    let e = (*end as usize).min(label.len());
                    let (s, e) = if s <= e { (s, e) } else { (e, s) };
                    // Snap to char boundaries so slicing the label is always valid (AC-008).
                    let s = snap_to_char_boundary(&label, s);
                    let e = snap_to_char_boundary(&label, e);
                    let text = label.get(s..e).unwrap_or("").to_owned();
                    (text, Some(s..e))
                }
                lsp_types::ParameterLabel::Simple(name) => {
                    let range = substring_range_from(&label, name, search_from);
                    if let Some(r) = &range {
                        search_from = r.end;
                    }
                    (name.clone(), range)
                }
            };
            parameters.push(ParamSpan {
                label: text,
                range_in_label: range,
            });
        }
    }
    let documentation = sig.documentation.as_ref().map(documentation_to_string);
    SignatureInfo {
        label,
        parameters,
        documentation,
    }
}

/// Flatten an `lsp_types::Documentation` (plain string or markup) to a display string.
fn documentation_to_string(doc: &lsp_types::Documentation) -> String {
    match doc {
        lsp_types::Documentation::String(s) => s.clone(),
        lsp_types::Documentation::MarkupContent(m) => m.value.clone(),
    }
}



pub fn signature_label_runs(sig: &SignatureInfo, active_param: usize) -> Vec<(String, bool)> {
    let active_range = sig
        .parameters
        .get(active_param)
        .and_then(|p| p.range_in_label.clone());
    let Some(range) = active_range else {
        return vec![(sig.label.clone(), false)];
    };
    // Defensive clamp so a stale range never slices out of bounds (AC-008).
    let start = snap_to_char_boundary(&sig.label, range.start.min(sig.label.len()));
    let end = snap_to_char_boundary(&sig.label, range.end.min(sig.label.len()));
    if start >= end {
        return vec![(sig.label.clone(), false)];
    }
    let mut runs = Vec::with_capacity(3);
    if start > 0 {
        runs.push((sig.label[..start].to_owned(), false));
    }
    runs.push((sig.label[start..end].to_owned(), true));
    if end < sig.label.len() {
        runs.push((sig.label[end..].to_owned(), false));
    }
    runs
}



fn substring_range_from(haystack: &str, needle: &str, from: usize) -> Option<Range<usize>> {
    if needle.is_empty() || from > haystack.len() {
        return None;
    }
    let from = snap_to_char_boundary(haystack, from.min(haystack.len()));
    let rel = haystack.get(from..)?.find(needle)?;
    let start = from + rel;
    Some(start..start + needle.len())
}

/// Snap a byte index to the nearest char boundary at or before it (so slicing the label is always
/// valid even when an LSP `LabelOffsets` lands mid-codepoint — AC-008, never panics).
fn snap_to_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_runs_emphasize_the_active_parameter() {
        // `LabelOffsets`-style explicit spans: active parameter 1 ('b: i32') is the emphasized run.
        let sig = SignatureInfo {
            label: "fn add(a: i32, b: i32) -> i32".to_owned(),
            parameters: vec![
                ParamSpan {
                    label: "a: i32".into(),
                    range_in_label: Some(7..13),
                },
                ParamSpan {
                    label: "b: i32".into(),
                    range_in_label: Some(15..21),
                },
            ],
            documentation: None,
        };
        let runs = signature_label_runs(&sig, 1);
        // The emphasized run is exactly 'b: i32'.
        let active: Vec<&String> = runs.iter().filter(|(_, a)| *a).map(|(t, _)| t).collect();
        assert_eq!(active, vec![&"b: i32".to_owned()]);
        // Reassembling the runs reproduces the whole label (no gaps/overlaps).
        let joined: String = runs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, sig.label);
    }

    #[test]
    fn simple_label_form_resolves_repeated_names_to_distinct_spans() {
        // RISK-005 / MC-005: two params named 'x' must map to DISTINCT spans (left-to-right consume).
        let sig = SignatureInfo::from_label_and_param_strings(
            "f(x: i32, x: i32)".to_owned(),
            vec!["x: i32".to_owned(), "x: i32".to_owned()],
        );
        let r0 = sig.parameters[0].range_in_label.clone().unwrap();
        let r1 = sig.parameters[1].range_in_label.clone().unwrap();
        assert_ne!(r0, r1, "repeated param names resolve to distinct spans");
        assert!(r0.end <= r1.start, "the second match is after the first");
        // The active run for param 1 is the SECOND 'x: i32'.
        let runs = signature_label_runs(&sig, 1);
        let active_start = sig
            .label
            .find("x: i32")
            .map(|i| i + "x: i32".len())
            .unwrap();
        let active_text: String = runs
            .iter()
            .filter(|(_, a)| *a)
            .map(|(t, _)| t.clone())
            .collect();
        assert_eq!(active_text, "x: i32");
        assert!(
            r1.start >= active_start,
            "param 1 maps to the second occurrence"
        );
    }

    #[test]
    fn from_lsp_parses_active_parameter_and_both_label_forms() {
        // A SignatureHelp with one signature, active_parameter = 1, mixing both ParameterLabel forms.
        let help = lsp_types::SignatureHelp {
            signatures: vec![lsp_types::SignatureInformation {
                label: "fn add(a: i32, b: i32) -> i32".to_owned(),
                documentation: Some(lsp_types::Documentation::String("Adds two numbers".into())),
                parameters: Some(vec![
                    lsp_types::ParameterInformation {
                        // Offsets form for 'a: i32'.
                        label: lsp_types::ParameterLabel::LabelOffsets([7, 13]),
                        documentation: None,
                    },
                    lsp_types::ParameterInformation {
                        // Simple form for 'b: i32'.
                        label: lsp_types::ParameterLabel::Simple("b: i32".into()),
                        documentation: None,
                    },
                ]),
                active_parameter: None,
            }],
            active_signature: Some(0),
            active_parameter: Some(1),
        };
        let state = SignatureHelpState::from_lsp(&help, 42).expect("one signature -> Some");
        assert_eq!(
            state.active_parameter, 1,
            "active parameter parsed from the response"
        );
        assert_eq!(state.source, SignatureSource::Lsp);
        assert_eq!(state.anchor_byte, 42);
        let sig = state.active().unwrap();
        // Both forms resolved to a non-empty span.
        assert_eq!(sig.parameters[0].range_in_label, Some(7..13));
        assert!(sig.parameters[1].range_in_label.is_some());
        // The active run is 'b: i32'.
        let runs = signature_label_runs(sig, state.active_parameter);
        let active: String = runs
            .iter()
            .filter(|(_, a)| *a)
            .map(|(t, _)| t.clone())
            .collect();
        assert_eq!(active, "b: i32");
        assert_eq!(sig.documentation.as_deref(), Some("Adds two numbers"));
    }

    #[test]
    fn from_lsp_empty_signatures_is_none() {
        // An empty SignatureHelp -> None so the caller falls back to code-nav (AC-003).
        let help = lsp_types::SignatureHelp {
            signatures: vec![],
            active_signature: None,
            active_parameter: None,
        };
        assert!(SignatureHelpState::from_lsp(&help, 0).is_none());
    }

    #[test]
    fn from_lsp_clamps_out_of_range_indices_without_panic() {
        // AC-008: out-of-range active indices clamp instead of panicking.
        let help = lsp_types::SignatureHelp {
            signatures: vec![lsp_types::SignatureInformation {
                label: "f(a)".to_owned(),
                documentation: None,
                parameters: Some(vec![lsp_types::ParameterInformation {
                    label: lsp_types::ParameterLabel::Simple("a".into()),
                    documentation: None,
                }]),
                active_parameter: None,
            }],
            active_signature: Some(9), // out of range
            active_parameter: Some(9), // out of range
        };
        let state = SignatureHelpState::from_lsp(&help, 0).unwrap();
        assert_eq!(
            state.active_signature, 0,
            "out-of-range signature index clamped"
        );
        assert_eq!(
            state.active_parameter, 0,
            "out-of-range parameter index clamped"
        );
    }

    #[test]
    fn label_offsets_out_of_bounds_does_not_panic() {
        // A server sending offsets past the label length must clamp (AC-008).
        let sig = lsp_types::SignatureInformation {
            label: "f(a)".to_owned(),
            documentation: None,
            parameters: Some(vec![lsp_types::ParameterInformation {
                label: lsp_types::ParameterLabel::LabelOffsets([2, 999]),
                documentation: None,
            }]),
            active_parameter: None,
        };
        let info = signature_info_from_lsp(&sig);
        // The span is clamped to the label length; slicing never panicked.
        let range = info.parameters[0].range_in_label.clone().unwrap();
        assert!(range.end <= info.label.len());
    }

    #[test]
    fn overload_cycling_wraps_and_clamps_active_parameter() {
        let mut state = SignatureHelpState {
            signatures: vec![
                SignatureInfo::from_label_and_param_strings(
                    "f(a, b)",
                    vec!["a".into(), "b".into()],
                ),
                SignatureInfo::from_label_and_param_strings("f(a)", vec!["a".into()]),
            ],
            active_signature: 0,
            active_parameter: 1,
            anchor_byte: 0,
            source: SignatureSource::Lsp,
        };
        // Switch to the second overload (1 param): the active parameter clamps from 1 to 0.
        state.select_next_signature();
        assert_eq!(state.active_signature, 1);
        assert_eq!(
            state.active_parameter, 0,
            "active param clamped to the smaller overload"
        );
        // Wrap forward back to the first.
        state.select_next_signature();
        assert_eq!(state.active_signature, 0);
        // Wrap backward to the last.
        state.select_prev_signature();
        assert_eq!(state.active_signature, 1);
    }
}
