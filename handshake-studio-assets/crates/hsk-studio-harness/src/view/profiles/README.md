Output profile of the develop export (`view::develop`, lane SMB-HARNESS-DEVELOP-EXPORT), copied
unchanged from `crates/hsk-studio-prism/tests/profiles/` (same bytes as
`crates/hsk-studio-develop/tests/profiles/sRGB-v4.icc`). Upstream: saucecontrol/Compact-ICC-Profiles
commit `bdd84663061bc4ae95ca70decff54f581e27f702`, CC0 (license retained in `CC0-LICENSE`).

| File | Bytes | SHA256 | Use |
|---|---:|---|---|
| sRGB-v4.icc | 480 | c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353 | sRGB primaries + sRGB transfer curve: describes `develop_to_output` samples (sRGB OETF over linear light with sRGB primaries); embedded in exported PNG (iCCP) and JPEG (APP2) |
