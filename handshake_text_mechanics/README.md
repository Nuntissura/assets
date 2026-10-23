# handshake_text_mechanics

Reusable text buffer, cursor, Unicode layout, formatting-edit and jump-history mechanics. The package owns one `TextBuffer` and `Cursor` implementation. Native application policy and rendering stay in the host.

## Public modules

- `buffer` and `cursor`: text storage, positions and selections.
- `line_ops`: line-edit transformations.
- `virtual_lines`, `text_intl`, `render_decorations` and `word_wrap`: viewport, Unicode and decoration calculations.
- `formatting`: UTF-16 formatting positions, ranges and edit application against the same buffer.
- `navigation` and `jump_history`: buffer positions and cross-file back/forward history using the same cursor type.

The host supplies LSP transport, capability checks, formatting menus, asynchronous requests, undo transactions, diagnostic gutter markers and GUI events. Formatting coordinates use UTF-16; the separate rename implementation's byte-column contract is not interchangeable.

## Model use

Read `src/lib.rs` for exports and each module's public functions and owning tests before calling it. Create a `buffer::TextBuffer`, pass it to cursor or formatting operations, and retain returned errors. Keep file paths in jump entries so cross-file navigation restores the correct location. The crate performs no account, storage, network or process initialization.

From the containing Assets repository, the source-proof commands are:

```powershell
cargo check --manifest-path handshake_text_mechanics/Cargo.toml --locked --tests -j 1
cargo test --manifest-path handshake_text_mechanics/Cargo.toml --locked -j 1
```

Run these only through the assigned proof owner with the required gameplan and external artifact paths. Lock generation, compilation and tests have not been run during source assembly. Dependencies are `ecolor` without default features, `ropey`, `unicode-segmentation`, `unicode-linebreak` and `unicode-bidi`; there is no egui, gutter or LSP runtime dependency.

## Compatibility and recovery

This extraction preserves the existing formatter's newline, CR, surrogate-position, overlapping-edit and error behavior, and the existing history behavior. It does not add overlap rejection or transactional rollback. Hosts must not reinterpret extraction as corrected coordinate semantics or an undo boundary.

On an edit error, let the host handle its existing recovery or undo policy; do not assume every failure leaves the buffer unchanged. On dependency or API mismatch, retain the previous host implementation and pin. Adopt this package only after independent source proof, canonical publication and an independent reread of the immutable revision. Source review alone does not establish host compilation or runtime acceptance.
