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

Linear test derivatives (local fixtures, not upstream originals):

| Derived profile | Bytes | SHA256 |
|---|---:|---|
| sRGB-linear-v4.icc | 480 | 4813c25a76abcf572f146c5a36b56ed83526c82327ee9f863553bf1e99c8b64d |
| DisplayP3-linear-v4.icc | 480 | 138ca6af5c8709c8dab4123a48ba2509ac8c89e53004f0909fc948f87f37f3f7 |

Each derivative starts from the corresponding original above (baseline hashes retained).
Using zero-based byte offsets: rTRC/gTRC/bTRC share offset448; set each tag-table
size to big-endian16. Zero bytes456..479, then set byte461 to1: ICC `para` type0,
gamma signed fixed16.16 exactly1. Zero bytes84..99 (old ICC MD5 profile identifier).
All other bytes, matrices, metadata and profile length remain unchanged; trailing
old TRC storage is zero padding outside the new declared16-byte tag. Retain CC0
license and pinned upstream provenance. These profiles test validated linear transfer,
not a newly registered canonical profile or cross-host materialization receipt.

The linear-pair oracle reads signed fixed16.16 XYZ columns and computes
`inverse(destination_matrix) * source_matrix * RGB` in f64; gamma1 is asserted
from fixture bytes. Both transform directions, primaries, endpoints and interior
samples use tolerance0.0001 and require nonidentity plus unclamped out-of-gamut
output. It calls no moxcms parser/evaluator. ICC reference remains the standard above.
