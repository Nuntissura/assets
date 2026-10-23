Test-only ICC fixtures copied unchanged from `crates/hsk-studio-prism/tests/profiles/`
(see that README for derivation). Upstream: saucecontrol/Compact-ICC-Profiles commit
`bdd84663061bc4ae95ca70decff54f581e27f702`, CC0 (license retained in `CC0-LICENSE`).

| File | Bytes | SHA256 | Use |
|---|---:|---|---|
| sRGB-admitted-linear-v4.icc | 368 | 63fc75efa59e9499411634a4298e48fc0f45d073f214a1c1de35fd76241aed27 | working profile (linear, admitted route) |
| DisplayP3-admitted-linear-v4.icc | 368 | c94737d225dec435f0119910080395e63d5efc6a6cbe594507d0c84544becfde | output profile (linear, admitted route) |
| sRGB-v4.icc | 480 | c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353 | encoded-transfer profile: must be refused by the admitted route |

The profile-transform oracle reads only the XYZ colorant tags (s15Fixed16) of the fixtures and
computes `inverse(destination) * source * rgb` in f64; it never calls the engine parser.
