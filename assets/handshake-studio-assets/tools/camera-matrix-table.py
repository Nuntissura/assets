#!/usr/bin/env python3
"""Writes crates/hsk-studio-develop/src/camera_table.rs: DNG-shaped colour matrices per camera model,
fitted to the measured spectral sensitivities of rawtoaces-data (Academy Software Foundation,
Apache-2.0, https://github.com/AcademySoftwareFoundation/rawtoaces-data). Python standard library
only; data in, Rust source out; nothing is executed from the data.

  python3 tools/camera-matrix-table.py <rawtoaces-data checkout> crates/hsk-studio-develop/src/camera_table.rs

Method (lane SMB-DEVELOP-CAMERA-COLOUR): the camera (measured sensitivities) and the CIE 1931 2 degree
observer (rawtoaces-data cmf_1931) look at the 190 training reflectances of rawtoaces-data under CIE
Standard Illuminant A (CIE 15 formula) and under CIE D65 (CIE daylight from the S0/S1/S2 basis at
6504 K, the basis table and daylight-locus formula as in rawtoaces `define.h` / `rawtoaces_core.cpp`,
Apache-2.0, linearly interpolated to 5 nm). Per illuminant L the camera values are white balanced to
L (divided by the camera's response to L itself), and two white-preserving 3x3 matrices are fitted
by least squares in XYZ (closed form, row-wise Lagrange constraint):
  ForwardMatrix_L: white-balanced camera -> XYZ D50 (Bradford L -> D50), (1, 1, 1) -> D50 white;
  ColorMatrix_L  : XYZ (L-referred) -> camera native = diag(camera response to L) . inverse(F'_L),
                   F'_L the white-preserving fit to unadapted XYZ; scaled so its largest response to
                   the L white is 1.
DNG CalibrationIlluminant codes: 17 (Standard light A), 21 (D65). Fit quality (CIE76 in Lab D50 of
the forward fit over the training set) is written next to each entry. Aliases come from
rawtoaces-data's own data/aliases.json. No number comes from Adobe, dcraw, LibRaw or rawspeed.
"""
import json
import math
import os
import sys

WL = list(range(380, 781, 5))
D50 = (0.96422, 1.0, 0.82521)  # hsk_studio_develop::color::D50_XYZ
BRADFORD = ((0.8951, 0.2664, -0.1614), (-0.7502, 1.7135, 0.0367), (0.0389, -0.0685, 1.0296))
# CIE daylight basis S0, S1, S2 at 10 nm, 300..830 nm (rawtoaces define.h `s_series`, Apache-2.0).
S_SERIES = [
    (300, 0.04, 0.02, 0.0), (310, 6.0, 4.5, 2.0), (320, 29.6, 22.4, 4.0), (330, 55.3, 42.0, 8.5),
    (340, 57.3, 40.6, 7.8), (350, 61.8, 41.6, 6.7), (360, 61.5, 38.0, 5.3), (370, 68.8, 42.4, 6.1),
    (380, 63.4, 38.5, 3.0), (390, 65.8, 35.0, 1.2), (400, 94.8, 43.4, -1.1), (410, 104.8, 46.3, -0.5),
    (420, 105.9, 43.9, -0.7), (430, 96.8, 37.1, -1.2), (440, 113.9, 36.7, -2.6), (450, 125.6, 35.9, -2.9),
    (460, 125.5, 32.6, -2.8), (470, 121.3, 27.9, -2.6), (480, 121.3, 24.3, -2.6), (490, 113.5, 20.1, -1.8),
    (500, 113.1, 16.2, -1.5), (510, 110.8, 13.2, -1.3), (520, 106.5, 8.6, -1.2), (530, 108.8, 6.1, -1.0),
    (540, 105.3, 4.2, -0.5), (550, 104.4, 1.9, -0.3), (560, 100.0, 0.0, 0.0), (570, 96.0, -1.6, 0.2),
    (580, 95.1, -3.5, 0.5), (590, 89.1, -3.5, 2.1), (600, 90.5, -5.8, 3.2), (610, 90.3, -7.2, 4.1),
    (620, 88.4, -8.6, 4.7), (630, 84.0, -9.5, 5.1), (640, 85.1, -10.9, 6.7), (650, 81.9, -10.7, 7.3),
    (660, 82.6, -12.0, 8.6), (670, 84.9, -14.0, 9.8), (680, 81.3, -13.6, 10.2), (690, 71.9, -12.0, 8.3),
    (700, 74.3, -13.3, 9.6), (710, 76.4, -12.9, 8.5), (720, 63.3, -10.6, 7.0), (730, 71.7, -11.6, 7.6),
    (740, 77.0, -12.2, 8.0), (750, 65.2, -10.2, 6.7), (760, 47.7, -7.8, 5.2), (770, 68.6, -11.2, 7.4),
    (780, 65.0, -10.4, 6.8), (790, 66.0, -10.6, 7.0), (800, 61.0, -9.7, 6.4),
]


def mat_mul(a, b):
    return tuple(tuple(sum(a[i][k] * b[k][j] for k in range(3)) for j in range(3)) for i in range(3))


def mat_vec(a, v):
    return tuple(sum(a[i][k] * v[k] for k in range(3)) for i in range(3))


def mat_inv(m):
    (a, b, c), (d, e, f), (g, h, i) = m
    det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g)
    if abs(det) < 1e-15:
        raise ValueError("singular")
    return (
        ((e * i - f * h) / det, (c * h - b * i) / det, (b * f - c * e) / det),
        ((f * g - d * i) / det, (a * i - c * g) / det, (c * d - a * f) / det),
        ((d * h - e * g) / det, (b * g - a * h) / det, (a * e - b * d) / det),
    )


def diag(v):
    return ((v[0], 0.0, 0.0), (0.0, v[1], 0.0), (0.0, 0.0, v[2]))


def bradford(src, dst):
    s, d = mat_vec(BRADFORD, src), mat_vec(BRADFORD, dst)
    return mat_mul(mat_inv(BRADFORD), mat_mul(diag([d[k] / s[k] for k in range(3)]), BRADFORD))


def illuminant_a():
    c2 = 1.435e7
    return [100.0 * (560.0 / w) ** 5 * (math.exp(c2 / (2848.0 * 560.0)) - 1.0) / (math.exp(c2 / (2848.0 * w)) - 1.0) for w in WL]


def daylight(cct):
    t = cct
    x = -4.6070e9 / t**3 + 2.9678e6 / t**2 + 0.09911e3 / t + 0.244063 if t <= 7000 else -2.0064e9 / t**3 + 1.9018e6 / t**2 + 0.24748e3 / t + 0.237040
    y = -3.0 * x * x + 2.87 * x - 0.275
    m = 0.0241 + 0.2562 * x - 0.7341 * y
    m1 = (-1.3515 - 1.7703 * x + 5.9114 * y) / m
    m2 = (0.0300 - 31.4424 * x + 30.0717 * y) / m
    out = []
    for w in WL:
        lo = max(r for r in S_SERIES if r[0] <= w)
        hi = min(r for r in S_SERIES if r[0] >= w)
        f = 0.0 if hi[0] == lo[0] else (w - lo[0]) / (hi[0] - lo[0])
        s = [lo[k] + f * (hi[k] - lo[k]) for k in (1, 2, 3)]
        out.append(s[0] + m1 * s[1] + m2 * s[2])
    return out


def spectral(path):
    doc = json.load(open(path, encoding="utf-8"))
    data = doc["spectral_data"]["data"]["main"]
    return doc["header"], [data[str(w)] for w in WL]


def lab(xyz, white):
    def f(t):
        return t ** (1.0 / 3.0) if t > 216.0 / 24389.0 else (24389.0 / 27.0 * t + 16.0) / 116.0

    fx, fy, fz = (f(xyz[k] / white[k]) for k in range(3))
    return (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))


def white_preserving_fit(cam, xyz, white):
    """3x3 F minimising sum |F c - x|^2 with F (1,1,1) = white (row-wise Lagrange)."""
    ccT = [[sum(c[i] * c[j] for c in cam) for j in range(3)] for i in range(3)]
    g = mat_inv(ccT)
    g1 = mat_vec(g, (1.0, 1.0, 1.0))
    s = sum(g1)
    rows = []
    for r in range(3):
        xc = [sum(x[r] * c[j] for x, c in zip(xyz, cam)) for j in range(3)]
        f0 = mat_vec(g, xc)
        lam = (white[r] - sum(f0)) / s
        rows.append(tuple(f0[j] + lam * g1[j] for j in range(3)))
    return tuple(rows)


def fit_camera(ssf, cmf, refl, light):
    n = [sum(ssf[w][k] * light[w] for w in range(len(WL))) for k in range(3)]
    ky = 1.0 / sum(cmf[w][1] * light[w] for w in range(len(WL)))
    white = tuple(ky * sum(cmf[w][k] * light[w] for w in range(len(WL))) for k in range(3))
    cams, xyzs = [], []
    for p in range(len(refl[0])):
        cams.append(tuple(sum(ssf[w][k] * light[w] * refl[w][p] for w in range(len(WL))) / n[k] for k in range(3)))
        xyzs.append(tuple(ky * sum(cmf[w][k] * light[w] * refl[w][p] for w in range(len(WL))) for k in range(3)))
    adapt = bradford(white, D50)
    fwd = white_preserving_fit(cams, [mat_vec(adapt, x) for x in xyzs], D50)
    unadapted = white_preserving_fit(cams, xyzs, white)
    cm = mat_mul(diag(n), mat_inv(unadapted))
    peak = max(mat_vec(cm, white))
    cm = tuple(tuple(v / peak for v in row) for row in cm)
    errs = [math.dist(lab(mat_vec(fwd, c), D50), lab(mat_vec(adapt, x), D50)) for c, x in zip(cams, xyzs)]
    return cm, fwd, sum(errs) / len(errs), max(errs)


def rust_mat(m):
    return "[" + ", ".join(f"{v:.6f}" for row in m for v in row) + "]"


def main(root, out):
    _, cmf = spectral(os.path.join(root, "data/cmf/cmf_1931.json"))
    _, refl = spectral(os.path.join(root, "data/training/training_spectral.json"))
    lights = {17: illuminant_a(), 21: daylight(6504.0)}
    entries = []
    camdir = os.path.join(root, "data/camera")
    for name in sorted(os.listdir(camdir)):
        header, ssf = spectral(os.path.join(camdir, name))
        fits = {code: fit_camera(ssf, cmf, refl, light) for code, light in lights.items()}
        entries.append((header["manufacturer"], header["model"], header.get("license", ""), name, fits))
        print(f"{header['manufacturer']} {header['model']}: mean/max dE76 A {fits[17][2]:.2f}/{fits[17][3]:.2f} D65 {fits[21][2]:.2f}/{fits[21][3]:.2f}")
    aliases = json.load(open(os.path.join(root, "data/aliases.json"), encoding="utf-8"))["data"]["camera"]
    lines = [
        "// @generated by tools/camera-matrix-table.py from rawtoaces-data (Academy Software Foundation,",
        "// Apache-2.0; see this crate's NOTICE). Do not edit by hand: re-run the tool.",
        "// Per entry: make, model, ColorMatrix1/2 and ForwardMatrix1/2 (row-major) for DNG",
        "// CalibrationIlluminant 17 (A) and 21 (D65), mean/max CIE76 of the D65 forward fit on the 190",
        "// training patches, source file.",
        "use super::SpectralCamera;",
        "",
        "pub(super) const CAMERAS: &[SpectralCamera] = &[",
    ]
    for make, model, lic, name, fits in entries:
        if lic and lic != "Apache-2.0":
            raise SystemExit(f"{name}: licence {lic}")
        lines.append(
            f"    SpectralCamera {{ make: {json.dumps(make)}, model: {json.dumps(model)}, "
            f"color_matrix: [{rust_mat(fits[17][0])}, {rust_mat(fits[21][0])}], "
            f"forward_matrix: [{rust_mat(fits[17][1])}, {rust_mat(fits[21][1])}], "
            f"fit_de76_d65: [{fits[21][2]:.2f}, {fits[21][3]:.2f}], source: {json.dumps(name)} }},"
        )
    lines += ["];", "", "/// rawtoaces-data aliases: (make, alias model, make, table model, comment).",
              "pub(super) const ALIASES: &[(&str, &str, &str, &str, &str)] = &["]
    for make, models in sorted(aliases["model"].items()):
        for alias, target in sorted(models.items()):
            lines.append(f"    ({json.dumps(make)}, {json.dumps(alias)}, {json.dumps(target['make'])}, {json.dumps(target['model'])}, {json.dumps(target.get('comments', ''))}),")
    lines += ["];", "", "/// rawtoaces-data make aliases (file make -> table make).",
              "pub(super) const MAKE_ALIASES: &[(&str, &str)] = &["]
    for k, v in sorted(aliases["make"].items()):
        lines.append(f"    ({json.dumps(k)}, {json.dumps(v)}),")
    lines.append("];")
    open(out, "w", encoding="utf-8", newline="\n").write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
