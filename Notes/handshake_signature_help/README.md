# Signature help

`handshake_signature_help` provides signature state, LSP result conversion and argument scanning for the Notes kit. It contains no GUI, backend client, process or storage integration.

## Use

- `scanner::active_parameter_from_commas(text, open_paren_byte, cursor_byte)` counts top-level argument separators while tracking nested delimiters and string/character literals.
- `scanner::top_level_paren_args(label)` extracts a best-effort parameter list for a host-side fallback converter.
- `model::SignatureHelpState::from_lsp(help, anchor_byte)` converts an LSP response into optional signature state. Empty responses return `None`.
- `SignatureInfo::from_label_and_param_strings` resolves parameter spans, including repeated labels. `signature_label_runs` produces text runs and emphasis flags.
- State methods select overloads and clamp the active parameter. The host retains popup state and the call-site byte anchor.

The scanner uses the standard library. The model uses the standard library and `lsp-types` 0.95. Primary owns the package manifest and crate-root exports; source placement alone does not establish completed package assembly or compilation.

## Host boundary and limitations

LSP session requests, trigger keys, asynchronous delivery, backend symbol conversion, rendering, focus, AccessKit and popup lifecycle remain in the native host. `SignatureSource::CodeNavFallback` is data, not a backend dependency. A future host adapter must use a free conversion function or an explicitly designed wrapper; it cannot add an inherent implementation to the external state type.

This extraction preserves the existing scanner's best-effort delimiter, generic-angle, byte-offset and malformed-input behavior. It is not a full language parser. Callers supply valid call-site offsets consistent with the existing API; debug assertions remain. Parameter-label conversion preserves the existing boundary handling and results rather than introducing a new parsing policy.

Five scanner tests and seven model/LSP tests are relocated from the original implementation. They have not been run during source authoring. Backend fallback tests remain host-owned. Compilation, independent behavior proof, publication and immutable source reread precede separate pinned host adoption; none is established by this manual. On a missing signature result, the host decides whether to dismiss or use its existing fallback.
