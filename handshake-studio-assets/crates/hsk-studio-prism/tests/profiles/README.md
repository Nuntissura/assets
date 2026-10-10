Pinned public CC0 fixtures from saucecontrol/Compact-ICC-Profiles commit
`bdd84663061bc4ae95ca70decff54f581e27f702` (license retained here).

| Profile | Bytes | SHA256 |
|---|---:|---|
| sRGB-v4.icc | 480 | c56e1685d888f5edb92fe07f2750f387f8fe8e91b32ff8fb0b56bfbbb9458353 |
| DisplayP3-v4.icc | 480 | cb51de38e482ee974c0c76b9689e16aad04bad16e226fed2f30c842d15ff3a3d |

Source: https://github.com/saucecontrol/Compact-ICC-Profiles/tree/bdd84663061bc4ae95ca70decff54f581e27f702/profiles

The independent test oracle reads only these fixtures' XYZ/type3 parametric tags,
then applies ICC.1:2022 equations with f64 arithmetic. It never calls the engine
parser, matrix helpers or evaluators. Reference: https://www.color.org/specification/ICC.1-2022-05.pdf
Channel tolerance is 0.0001, including encoded fixed16.16 coefficient rounding
and the engine's f32 analytical evaluation; explicit nonidentity is also required.
This is a foundation oracle, not the full normative color-difference corpus or
cross-host materialization proof.
