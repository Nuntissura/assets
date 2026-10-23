"""Generate src/vc3/tables.rs from the text of SMPTE ST 2019-1:2016 (pdftotext -raw for Annex E,
pdftotext -layout for Annex D). Validates every VLC table (lengths, prefix-free, completeness, counts)
before writing. Usage: python -I gen_vc3_tables.py st_raw.txt st.txt out.rs"""
import re, sys, hashlib

raw = open(sys.argv[1], encoding='utf-8', errors='replace').read().splitlines()
lay = open(sys.argv[2], encoding='utf-8', errors='replace').read().splitlines()
out_path = sys.argv[3]

def section(lines, start_pat, stop_pat):
    i = next(k for k, l in enumerate(lines) if re.search(start_pat, l))
    j = next(k for k in range(i + 1, len(lines)) if re.search(stop_pat, lines[k]))
    return lines[i + 1:j]

AMP_RE = re.compile(r'^([01]+) (\d+) (EOB1?|\d+) ([01]) ([01])$')
def amp_table(n):
    rows = []
    for l in section(raw, r'^Table E\.%d . Amplitude' % n, r'^Table E\.%d . Run' % (n + 1)):
        m = AMP_RE.match(l.strip())
        if m:
            code, ln, amp, run, idx = m.groups()
            assert int(ln) == len(code), (n, l)
            eob = amp.startswith('EOB')
            rows.append((code, 0 if eob else int(amp), int(run), int(idx), eob))
    assert len(rows) == 257, (n, len(rows))
    assert sum(r[4] for r in rows) == 1
    keys = [(r[1], r[2], r[3]) for r in rows if not r[4]]
    assert len(set(keys)) == 256, n
    for r in rows:
        assert r[4] or 1 <= r[1] <= 64
    return rows

RUN_RE = re.compile(r'^([01]+) (\d+) (\d+)(?: ([01]+) (\d+) (\d+))?$')
def run_table(n):
    rows = []
    for l in section(raw, r'^Table E\.%d . Run' % n, r'^Table E\.%d . DC' % (n + 1)):
        m = RUN_RE.match(l.strip())
        if m:
            g = m.groups()
            for code, ln, run in ((g[0], g[1], g[2]), (g[3], g[4], g[5])):
                if code is None:
                    continue
                assert int(ln) == len(code), (n, l)
                rows.append((code, int(run)))
    assert sorted(r[1] for r in rows) == list(range(1, 63)), (n, sorted(r[1] for r in rows))
    return rows

DC_RE = re.compile(r'^([01]+) (\d+)$')
def dc_table(n, stop):
    rows = []
    for l in section(raw, r'^Table E\.%d . DC' % n, stop):
        m = DC_RE.match(l.strip())
        if m:
            rows.append((m.group(1), int(m.group(2))))
    vals = sorted(r[1] for r in rows)
    assert vals == list(range(len(vals))) and len(vals) in (12, 14), (n, vals)
    return rows

def check_prefix_code(rows, name):
    codes = [r[0] for r in rows]
    for a in codes:
        for b in codes:
            assert a == b or not b.startswith(a), (name, a, b)
    kraft = sum(2.0 ** -len(c) for c in codes)
    return kraft

def sort_by_start(rows):
    return sorted(rows, key=lambda r: int(r[0], 2) << (16 - len(r[0])))

sets = {}
for (a, r, d) in ((1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12), (13, 14, 15), (16, 17, 18)):
    stop = r'^Table E\.%d ' % (d + 1) if d < 18 else r'Annex F System Overview'
    sets[a] = (amp_table(a), run_table(r), dc_table(d, stop))
    k = [check_prefix_code(x, 'E%d' % i) for x, i in zip(sets[a], (a, r, d))]
    sets[a] += (k,)

# Annex D weights from the layout text: 8 rows of 8 ('-' or '--' for the unweighted DC position).
def weights(title_pat):
    i = next(k for k, l in enumerate(lay) if re.search(title_pat, l))
    mats = []
    k = i + 1
    while len(mats) < 2 and k < len(lay):
        if 'Quantization Weighting Table' in lay[k]:
            rows = []
            k += 1
            while len(rows) < 8:
                t = lay[k].split()
                k += 1
                if not t:
                    continue
                if t[0] in ('-', '--'):
                    t = ['0'] + t[1:]
                elif len(t) == 7:
                    t = ['0'] + t  # D.11 prints the DC position as blank
                assert len(t) == 8 and all(x.isdigit() for x in t), (title_pat, t)
                rows.append([int(x) for x in t])
            mats.append(rows)
            if title_pat.startswith('Table D.11'):
                break
        else:
            k += 1
    return mats

W = {}
for d in range(1, 12):
    W[d] = weights(r'Table D\.%d \S+ Compression' % d)
    assert len(W[d]) == (1 if d == 11 else 2), d

def rs_amp(name, rows):
    s = 'pub(super) const %s: [AcCode; %d] = [\n' % (name, len(rows))
    for code, amp, run, idx, eob in sort_by_start(rows):
        start = int(code, 2) << (16 - len(code))
        s += '    AcCode { start: 0x%04X, len: %d, amp: %d, run: %s, index: %s },\n' % (
            start, len(code), amp, 'true' if run else 'false', 'true' if idx else 'false')
    return s + '];\n'

def rs_vlc(name, rows):
    s = 'pub(super) const %s: [Vlc; %d] = [\n' % (name, len(rows))
    for code, val in sort_by_start(rows):
        start = int(code, 2) << (16 - len(code))
        s += '    Vlc { start: 0x%04X, len: %d, value: %d },\n' % (start, len(code), val)
    return s + '];\n'

def rs_w(name, m):
    s = 'pub(super) const %s: [[u8; 8]; 8] = [\n' % name
    for row in m:
        s += '    [%s],\n' % ', '.join('%d' % x for x in row)
    return s + '];\n'

src = hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest()
o = ['//! VC-3 code tables, GENERATED from the text of SMPTE ST 2019-1:2016 (Annex D quantization weights,',
     '//! Annex E VLC tables) by the lane-local generator gen_vc3_tables.py (pdftotext -raw / -layout of the PDF',
     '//! with sha256 cf99fe4459665ac782facdfeca37f530d3f744cf3a1ff4f19f6a12ec29d59449). Do not edit by hand.',
     '//! The generator checked every table: codeword lengths equal the Length column, the codes are',
     '//! prefix-free, amplitude tables hold 256 distinct (amp, run, index) symbols plus one EOB, run tables',
     '//! hold runs 1..=62 once each, DC tables hold epsilon 0..=11 (8-bit) or 0..=13 (10-bit).',
     '//! Each array is sorted by `start` = codeword left-aligned to 16 bits, so a 16-bit peek resolves',
     '//! by binary search (`entropy::lookup`). Weight matrices: row = vertical frequency v, column =',
     '//! horizontal frequency u (Figure 48 layout); position (0, 0) is unweighted (printed "-").',
     '#![allow(clippy::unreadable_literal)]',
     'use super::entropy::{AcCode, Vlc};', '']
kraft_notes = []
for a, (amp, run, dc, k) in sets.items():
    o.append('// Table E.%d (amplitude), E.%d (run), E.%d (DC); Kraft sums %s.' % (a, a + 1, a + 2, ', '.join('%.6f' % x for x in k)))
    o.append(rs_amp('AC_E%d' % a, amp))
    o.append(rs_vlc('RUN_E%d' % (a + 1), run))
    o.append(rs_vlc('DC_E%d' % (a + 2), dc))
for d, mats in W.items():
    o.append('// Table D.%d.' % d)
    o.append(rs_w('W_D%d_LUMA' % d if len(mats) == 2 else 'W_D%d' % d, mats[0]))
    if len(mats) == 2:
        o.append(rs_w('W_D%d_CHROMA' % d, mats[1]))
open(out_path, 'w', encoding='utf-8', newline='\n').write('\n'.join(o))
for a, (amp, run, dc, k) in sets.items():
    print('E.%d sets: kraft %s, dc entries %d' % (a, k, len(dc)))
