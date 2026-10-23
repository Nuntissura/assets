Test-only ICC profiles, copied unchanged from `crates/hsk-studio-prism/tests/profiles/`.

| File | Bytes | SHA256 |
|---|---:|---|
| sRGB-v4.icc | 480 | c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353 |
| sRGB-linear-v4.icc | 480 | 4813c25a76abcf572f146c5a36b56ed83526c82327ee9f863553bf1e99c8b64d |

`sRGB-v4.icc` is CC0 (saucecontrol/Compact-ICC-Profiles, commit
bdd84663061bc4ae95ca70decff54f581e27f702); `sRGB-linear-v4.icc` is the derivative documented in the
Prism fixtures README (parametric curve set to gamma 1). Licence text retained in `CC0-LICENSE`.
Used as the embedded source profile and the linear destination of `PrismConverter` in tests.
