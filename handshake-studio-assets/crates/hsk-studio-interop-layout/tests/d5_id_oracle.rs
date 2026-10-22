//! D5 InDesign oracle comparison (lane SMB-D5-AI-ID-AE-COMPARE): the composer probes of
//! `oracle-session/hsk_oracle_indesign.jsx` (ID-01 width x composer x hyphenation x justification
//! sweep, ID-02 spacing limits, ID-03 IDML exports, ID-04 threaded frames) driven through Studio:
//! InDesign's own ID-03 IDML is imported (`import_idml`), the paragraph is shaped by Type with the
//! exact Arial file InDesign used (`<oracle-corpus>/host-fonts/arial.ttf`, pinned), and
//! `layout::reflow_typeset` composes it with the case's composer, hyphenation, alignment and
//! spacing limits on a frame of the case's width.
//!
//! Compared: line starts (story character index of every line; TEXT is ASCII, so byte index =
//! character index) and, on lines whose start and end agree, every character's x
//! (`Character.horizontalOffset`, points) against Studio's x from the shaped advances plus the
//! line's justification (word scale on spaces, glyph scale on the other glyphs, letter gap). Breaks
//! MATCH only when every line start agrees; x MATCH within 0.001 pt on every character.
//!
//! "ff" of "differences" (lane SMB-TYPE-FF-LIGATURE): InDesign sets Arial's cmap ligature uniFB00
//! (1094 units) and reports its components at ligature carets: the second f at half the ligature
//! (547 units, times the glyph scale) plus the glyph's letter gap, the next glyph at 1094 plus the
//! gap. Type composition v2 forms the same ligature; Studio's carets here follow that rule. Split by
//! a hyphen ("dif-" / "ferences", ID-01 para_hyph_left_w210 and leftjust_w150) each side is set
//! with its nominal f (569 units): the layout line's `end_part` / `start_part`. Before the
//! ligature fix the ff line was held to 0.1 pt (Studio kerned f-f to 1101 units); that exception
//! is removed.
//!
//! InDesign settings mapped: `hyphenateWordsLongerThan 5` behaves as "words of at least 5 letters"
//! (the oracle hyphenates the 5-letter "ev-ery", ID-01 para_hyph_leftjust_w240 line 0), so
//! `min_word_len` is 5.
//!
//! Verdicts are printed per variant (`D5_VERDICT:`); a MISMATCH fails unless listed with a reason in
//! `tests/d5_id_expected_mismatch.txt`.
mod common;

use common::text_shaping::{FontFile, Seg, shape};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{FirstBaseline, GeometryUnit, Length, StoryContent, StudioDocument, StudioTextStory};
use hsk_studio_interop_layout::layout::{
    AdobeParams, Alignment, Composer, HyphenationParams, Hyphenator, JustifyParams, LayoutProposal, Limits as ReflowLimits,
    ReflowRequest, ShapedParagraph, SpaceParams, Spacing, StyleSource, TypesetOptions, reflow_typeset,
};
use hsk_studio_interop_layout::{Limits, SequentialIdMint, import_idml, import_type_styles};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const DIR: &str = "adobe-session/indesign";
const FONT_DIR: &str = "host-fonts";
const FONT_PIN: &str = "b3658eadae55e682b5f69eb64c439c1ecc8f196c0bb8d4756d145d13bc86476a";
const EXPECTED: &str = include_str!("d5_id_expected_mismatch.txt");
const X_TOL: f64 = 0.001;
const SIZE: f64 = 12.0;
const LEADING: f64 = 14.4;

/// The probe paragraph, byte-identical to `TEXT` in hsk_oracle_indesign.jsx (checked against the
/// imported IDML story).
const TEXT: &str = "Typographic composition decides where every line of a paragraph ends, and a composer that looks at the whole paragraph can trade tighter word spacing against fewer hyphens. Extraordinarily long words such as internationalization, counterrevolutionary and incomprehensibilities give the hyphenator real choices, while short words keep the lines honest. Measure the result carefully, because small differences in spacing limits move entire line breaks.";

/// SHA-256 of every InDesign 21.6 export read (session 2026-10-16).
const PINS: &[(&str, &str)] = &[
    ("ID-01__para_hyph_left_w150.json", "f92d238e0a636a2cb3362931bb3e6dcb8ae16f0618d333a1f65b6c4836923557"),
    ("ID-01__para_hyph_left_w180.json", "71fa71208998e01b67d5793ce97c13991228cbb1c804a47e39c869983947dc20"),
    ("ID-01__para_hyph_left_w210.json", "18a22741049986219922cd1e1edfa55c585a733cf3af18a96fa08942400ebff6"),
    ("ID-01__para_hyph_left_w240.json", "d089c7b92ca495052ca6543cca6111330d8ce5ac566151ea7f0f5acb18b96860"),
    ("ID-01__para_hyph_left_w270.json", "64b2d2becc9e935b7c200a97c123204eddcbebba137eebb4eb5d618d0d20b412"),
    ("ID-01__para_hyph_left_w300.json", "84d6853e37e6cb907c0c05e53d1a47be3dd2a41698d253f07d6730ef12792333"),
    ("ID-01__para_hyph_left_w330.json", "780cd34ffe7e7dd9a2052b22a186ba41e1a05dc205d2f338a8308d56f721c601"),
    ("ID-01__para_hyph_left_w360.json", "7ca2ee5d4c1c88ea8ce25764cecfb6b2f2fd64cb3c8150b53d0e2fc88cc2c011"),
    ("ID-01__para_hyph_left_w390.json", "9dc4654f0dd6f69d736e124256e7381f8bba913cfbdc3e780cadcd0a6292989b"),
    ("ID-01__para_hyph_left_w420.json", "10d09a644a5c68dfb4de5f38b40346e3fad000953fb2559d013cabfe74b2ddf9"),
    ("ID-01__para_hyph_leftjust_w150.json", "177f7131d1d37bcb5011c85fb5b32a9e8294158e999b46883182a83481fd8df0"),
    ("ID-01__para_hyph_leftjust_w180.json", "912a386fe18bab13d8d16a3dcc5d732b063abc4ca3319cf5b134034706d3b145"),
    ("ID-01__para_hyph_leftjust_w210.json", "1d0a3ceea56bf2c782c9f8d3e4659d3412a76bc864c96541f82a327d9c07d586"),
    ("ID-01__para_hyph_leftjust_w240.json", "bcf5787cc6782d31964c18ae78df6e9a104223cb29bea2140a1ebf516a6cbaeb"),
    ("ID-01__para_hyph_leftjust_w270.json", "ceae874acc62567fd9ea436dee5623d6432edf4f56c398e3575f2a238b12fa3d"),
    ("ID-01__para_hyph_leftjust_w300.json", "34f0da8e45f469aaa64e2cfc2ec54f70799be089fddbdcefc23d15c04d1268a3"),
    ("ID-01__para_hyph_leftjust_w330.json", "9e04039c8cadd71ace4fa3e87c956b2b7da0715bd30eaece670772a93345b275"),
    ("ID-01__para_hyph_leftjust_w360.json", "7a55a8d4bead724df2aaa109b6050ef8b5679ab995ff19f31cc48841a227180d"),
    ("ID-01__para_hyph_leftjust_w390.json", "ef6243fa18c49a6b640c81ab49652669b1c535403e3f8dce0682f461935cae01"),
    ("ID-01__para_hyph_leftjust_w420.json", "f44c28370207209cca45e4b722fef12e220d7224ae89259a8547968af1a00e7d"),
    ("ID-01__para_nohyph_left_w150.json", "893722a479985986942e5b2cfd3e2100f0970440558ac12ed9e5d121a1176595"),
    ("ID-01__para_nohyph_left_w180.json", "8c58d21106d49ddd6ce9ee9368575e1c12701b3f84033bbc100874d61ea95622"),
    ("ID-01__para_nohyph_left_w210.json", "437c537a12d9c80b691be841158c2f5d87cf9412a3a6f0b7fc5cd4cd5d4420f2"),
    ("ID-01__para_nohyph_left_w240.json", "34488626b1e21d9ed81394fca83a6377eef380240c0f8a84c0e8af1cdff464ef"),
    ("ID-01__para_nohyph_left_w270.json", "be5ee918f976e1a57a3b14b0958e62d41a5a39f8ca28daa19b72f93f19e99ce3"),
    ("ID-01__para_nohyph_left_w300.json", "9ee6b9d501d8a886184dbad535eae8438b122203364628429b8a6b04ba0143e1"),
    ("ID-01__para_nohyph_left_w330.json", "cdde3bc45bbd257c72af37aeb71ca1023ad2b607bd16eeed09f43106ab168c46"),
    ("ID-01__para_nohyph_left_w360.json", "ed78b05ba60c7f2413d61d0284155d01eaa883e26822dcd4a8d92bc834cdc9c0"),
    ("ID-01__para_nohyph_left_w390.json", "7b96528a7fd9f26658a0bc09a377fda639967e1394e473a356c26f2d2bf4079f"),
    ("ID-01__para_nohyph_left_w420.json", "a2675998cff2417018751c271a2ed89307c41da8967314549d7a36e4056817df"),
    ("ID-01__para_nohyph_leftjust_w150.json", "3edc17568ab921a13dabe8285a9040217f48de94afb2b417c15621b65eeb0318"),
    ("ID-01__para_nohyph_leftjust_w180.json", "40d7902c9aae9aac1a6f46c78c2f1ade18b501d2bf67130afa7c033784aeec7e"),
    ("ID-01__para_nohyph_leftjust_w210.json", "6be5dcb0e99eaa57bebbd6599bdc63cbd8f6d59693441de22cafc2ef9de9d54e"),
    ("ID-01__para_nohyph_leftjust_w240.json", "0b033090d3838d2b51c28cc6b49920d767c10ef15180a64906d64540b07f19bc"),
    ("ID-01__para_nohyph_leftjust_w270.json", "3eeb749f6b3eb02f0fb849fa4bcb91bd56aae50271176a13b310a67d87e84294"),
    ("ID-01__para_nohyph_leftjust_w300.json", "9e9c8370eb472dd65ffea88bfc50f2c205ec0d22add934e70058a327c2885db3"),
    ("ID-01__para_nohyph_leftjust_w330.json", "7d403bc4b7440e96de3c9dd81f0bf7bffd7d7426894a73285b24e747e8e22953"),
    ("ID-01__para_nohyph_leftjust_w360.json", "d6a2beccfb0c86deab963abfbaa59ffb625ff786ffea1fce7eafa50f5af3f0cb"),
    ("ID-01__para_nohyph_leftjust_w390.json", "8e6d74b51a7a9330c28e05d2e0fc259e53d7b56bf10e5b7162dba14dae66853d"),
    ("ID-01__para_nohyph_leftjust_w420.json", "7cc7a25feacde14b320f4c8d3a7bb46e20dc0f2f5b9d8101a2e2c7aadfcf69aa"),
    ("ID-01__single_hyph_left_w150.json", "4486ae1b0a8606fe8fa8f848c01bc9ee9fee79afffa1f5b6709cc16b561632ae"),
    ("ID-01__single_hyph_left_w180.json", "8582c621428007148dd8b7076199c7b4edb08b26757eacdfc6f564b28eab464e"),
    ("ID-01__single_hyph_left_w210.json", "15e33ee54d4e168dc865de4aa96c8b647e084dcbccd0228007fc089b013c877c"),
    ("ID-01__single_hyph_left_w240.json", "5ba795363892b466fdabd4e3fb66f6e9a91b86f10b5a3794dcd8e92ef9624fe3"),
    ("ID-01__single_hyph_left_w270.json", "d42e6fe1cb3317701301b135d17725d3a1ade5e9ea6fcd0bd7ea93e50e92118a"),
    ("ID-01__single_hyph_left_w300.json", "5d8ad54627ba9426ff9d6d75392066506bd6160dfef84017256d64917f6bc225"),
    ("ID-01__single_hyph_left_w330.json", "5b13b36d42d61ce1c942f2dead489fc1d13ef079a956f9506bc27d656b5d370f"),
    ("ID-01__single_hyph_left_w360.json", "4a55c068dc5e3d380d7f00fbae4e32bf85ca6fdf5d784cd31125e4ff80438eda"),
    ("ID-01__single_hyph_left_w390.json", "a61618dc2d210d3121f97e785c65f9a51d11e51d052d711bc649a82e5a00c255"),
    ("ID-01__single_hyph_left_w420.json", "3a7b1dbe62d444c6385b935d0d87359ec3106109b61a571565056ebc61cfb86d"),
    ("ID-01__single_hyph_leftjust_w150.json", "ce1b3b693ddb416275a28bac62d3723b7c5552f55220659ce3fe4f6329716098"),
    ("ID-01__single_hyph_leftjust_w180.json", "2c72b33ac0c878f2dfb4c270fc57214049de0043c58baec7b3c4cd46a24958ab"),
    ("ID-01__single_hyph_leftjust_w210.json", "1ed0c463d2ceb15c5e0f5bdc07082afacb65fa8f23be7e9dcbb3617168fe753c"),
    ("ID-01__single_hyph_leftjust_w240.json", "1ca8902dcb8e3ae7e5fc797f81b39ed9991180382e64ebf4f723108c335b7990"),
    ("ID-01__single_hyph_leftjust_w270.json", "775871966b19bfb72c93e55f5562f34fe86009a7aae274e159540fb686e5da35"),
    ("ID-01__single_hyph_leftjust_w300.json", "62a1431e22faddcac61d4e898dd1ead82fa3bcd3dee30a1f97301c65c9198b31"),
    ("ID-01__single_hyph_leftjust_w330.json", "a8dfc715c69b1ce3b5221f6a6b5106ac74fc5b4b6cf9f4a72b0fc57d62e76923"),
    ("ID-01__single_hyph_leftjust_w360.json", "26c241d730525da9cd2b135ca9d06d3a6a62428f23aa52a12144b0ae891bc0b6"),
    ("ID-01__single_hyph_leftjust_w390.json", "69ac8f74c797c78c801eafe531f604f65925ccd77002caa47f3b4f3290ef2f50"),
    ("ID-01__single_hyph_leftjust_w420.json", "a2e239fc34e1cd3b5bfa03a92cdb4670af1ae116f3a381e07249ca8cd741a20a"),
    ("ID-01__single_nohyph_left_w150.json", "05cedc4a3bcb2bca42997b551cd58687c8d70b7fb299c27bc3f2bda35f246c6b"),
    ("ID-01__single_nohyph_left_w180.json", "df8aa235ddfdf32a4cf0e2d0f80b8a8a0197f4fb1288c4621c7a7967cb1affce"),
    ("ID-01__single_nohyph_left_w210.json", "d55d8e884153d35100f0f0651655e5cf6f5f05890ebc5bf411409e694a18439b"),
    ("ID-01__single_nohyph_left_w240.json", "1a770125c192f7e5c5afc41935809b0ddd669fe332a5ca1722e92a20044dfd5a"),
    ("ID-01__single_nohyph_left_w270.json", "60d1a223e85a2f026b778ebfc135d420b301a13b5b5362eb874837540a6c692d"),
    ("ID-01__single_nohyph_left_w300.json", "c88fd27ce041633e571ea26fda0a6de7281a14cd698782c01972d0e86f7f96ce"),
    ("ID-01__single_nohyph_left_w330.json", "4d41a6a044d50e5ba7bc46463582c3a22c372ddd6201a3b4f3cf3a8ab3c7a49b"),
    ("ID-01__single_nohyph_left_w360.json", "acce17d0e907a50730c13411c439379e5ba4a853adfd763b00c03204e192e184"),
    ("ID-01__single_nohyph_left_w390.json", "92ca1ebb6816f130efb4ad3875b00cfde81d099b2626a10cc97c662f504c16cb"),
    ("ID-01__single_nohyph_left_w420.json", "f96de174ed7b48b4168141b510625a2841d6f1004cddd35d23b2f807ab73c40f"),
    ("ID-01__single_nohyph_leftjust_w150.json", "18e6b7abe571da2ea08fc6117ce5303e078bdafab0a6c979a9cbcc312786ff33"),
    ("ID-01__single_nohyph_leftjust_w180.json", "2f74707c958a155eb222d55fd64dd69a32397ba3e89bf2ca49e9b8e597480d41"),
    ("ID-01__single_nohyph_leftjust_w210.json", "6885bb075a7d31ed36bd8739db0d3267d0bf997fde4a70bc7c9f528a5ee70a03"),
    ("ID-01__single_nohyph_leftjust_w240.json", "94aaae9a76a8fcbf03b12667dcc2966af562e37e05628b1ab5fb26d43c1b0f65"),
    ("ID-01__single_nohyph_leftjust_w270.json", "e188b8d8f53b138d12c931150f6982f6493b79fe828d2d22b42a70093d58c718"),
    ("ID-01__single_nohyph_leftjust_w300.json", "2d49a72932971bf59e3aa82366570322d579b7ec49029192522621c86e46368e"),
    ("ID-01__single_nohyph_leftjust_w330.json", "063678c5e3e397f4c1d8557390d4aa1150a0f8ee00703f2ce3c796bd1c0f6ace"),
    ("ID-01__single_nohyph_leftjust_w360.json", "18ba7d089e1d578b8d570ee1b2edcc18dfdadefc27d397c9f65b1cb1bedd960b"),
    ("ID-01__single_nohyph_leftjust_w390.json", "1e3b4c4003bd7fa57549ae5d6b588cc5a6da42b9cd2addb5cea07dcc5b394e48"),
    ("ID-01__single_nohyph_leftjust_w420.json", "1029388c4a2c5b58ac6ae360a8c8ffd1201c9decf510094830ae37fa63a5d401"),
    ("ID-02__para_ws0_100_300.json", "cd892090ad94eda5d77d061a36c794a6a40d538190315e68d7914c773b0d8d2b"),
    ("ID-02__para_ws100_100_100.json", "c4a591321a71249fdff80c79a64c8b86d93b53fcf509d3b4f976440ad3a02a44"),
    ("ID-02__para_ws100_100_100_ls-5_0_5_gs90_100_110.json", "7a52023835f1778e27e7c825ce9e83b5b637dc5004b5e630b2361340aaf3778e"),
    ("ID-02__para_ws50_100_200.json", "aa268b89515605d13dc235381ffa9bc59a91da5759206606268ae41027191b85"),
    ("ID-02__para_ws80_100_133_gs95_100_105.json", "7226bb54d16588e05a4f4eb91ce8802320b14f96a9882b96a62c5337f6a6b793"),
    ("ID-02__para_ws80_100_133_ls-3_0_3.json", "ee361ae7fa6c028a2385ab8fdf3db14e78d102ad03ca5e0fb01e50c2da74cf0f"),
    ("ID-02__single_ws0_100_300.json", "68ae82964d65b5cfc29c323be5e849e219a4a0d329e32f9c4815f43648e1b967"),
    ("ID-02__single_ws100_100_100.json", "21a58cef685b1c995ededd5247b6f18ddc223f2a6006ac3d3c73ec1418bca9d0"),
    ("ID-02__single_ws100_100_100_ls-5_0_5_gs90_100_110.json", "e30ca7d12d5da8dc6a11d3b1610f223f7f149fcda688c051d066d7e6dd3f2914"),
    ("ID-02__single_ws50_100_200.json", "7911643964e3d6f1120dc663d49a6d40ff6e2c8cfd55c7ec095fc69905279d76"),
    ("ID-02__single_ws80_100_133_gs95_100_105.json", "4ed11f205db94257f71a61075c6123bcfb6eff0f0028a8724e9ae7770d1f6c91"),
    ("ID-02__single_ws80_100_133_ls-3_0_3.json", "4aed56d8472e3810378a3ab713fd071fde8355bfe30504ad4b03cc17b2a02c5f"),
    ("ID-03__para_hyph_w240.json", "f8dd8673a33467152753b317811a4ac9fca80865933ffe08f0691f200a084017"),
    ("ID-03__para_nohyph_w240.json", "7df9c922b3f7b13a4a0575ff3a60161cdb51f2f03d61d426b2eb0597e233be45"),
    ("ID-03__single_hyph_w240.json", "89611a5f62abf6ea61d3253f986383f6bb99777b18705f60e8a12ece828c33fc"),
    ("ID-04__two_linked_small_frames.json", "99d002d878c3d79924f8269e25f14a2fb1fabeed6518e6ee4d27f7f252643cc3"),
    ("ID-03__para_hyph_w240.idml", "950704901b55db6962fd4e0d20cece598bda265ac232de94baf158e731537820"),
    ("ID-03__para_nohyph_w240.idml", "afca70ccbb7a01e6a70cce1ceee8edfa5cf93f43c59163808abeadaf519484e6"),
    ("ID-03__single_hyph_w240.idml", "80fc23cfe8ee83fc778906f166aa6e25ba18bbd789af6ad88dbb28ccb84905d5"),
];

fn corpus_root() -> Option<PathBuf> {
    let ws = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cfg = std::fs::read_to_string(ws.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| {
        cfg.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_owned())
        })
    };
    let search = value("search")?;
    let max: usize = value("max-ancestors")?.parse().ok()?;
    let ws = ws.canonicalize().ok()?;
    ws.ancestors().skip(1).take(max).map(|a| a.join(&search)).find(|p| p.is_dir())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_pinned(root: &Path, dir: &str, file: &str, pin: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(root.join(dir).join(file)).map_err(|_| format!("{dir}/{file} absent"))?;
    let got = hex(&Sha256::digest(&bytes));
    if got != pin {
        return Err(format!("{dir}/{file} sha256 {got} != pinned {pin}"));
    }
    Ok(bytes)
}

fn oracle(root: &Path, file: &str) -> Result<Vec<u8>, String> {
    let pin = PINS.iter().find(|(f, _)| *f == file).map(|(_, h)| *h).ok_or(format!("{file} has no pin"))?;
    read_pinned(root, DIR, file, pin)
}

// ---------------------------------------------------------------------------------------------
// Verdict ledger (same contract as the AE/AI comparison tests).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    Match,
    Mismatch,
    NotSupported,
    NotProven,
}
impl Verdict {
    fn word(self) -> &'static str {
        match self {
            Verdict::Match => "MATCH",
            Verdict::Mismatch => "MISMATCH",
            Verdict::NotSupported => "NOT_SUPPORTED",
            Verdict::NotProven => "NOT_PROVEN",
        }
    }
}
struct Ledger {
    test: &'static str,
    expected: Vec<(String, String)>,
    rows: Vec<(String, String, Verdict, String)>,
}
impl Ledger {
    fn new(test: &'static str) -> Self {
        let expected = EXPECTED
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                let (head, _reason) = l.split_once('|')?;
                let mut it = head.split_whitespace();
                Some((it.next()?.to_owned(), it.next()?.to_owned()))
            })
            .collect();
        Self { test, expected, rows: Vec::new() }
    }
    fn record(&mut self, case: &str, variant: &str, verdict: Verdict, detail: String) {
        if verdict == Verdict::NotProven {
            println!("NOT_PROVEN: {} {case} {variant} reason={detail}", self.test);
        }
        println!("D5_VERDICT: {case} {variant} {} {detail}", verdict.word());
        self.rows.push((case.into(), variant.into(), verdict, detail));
    }
    fn finish(self) {
        let count = |v: Verdict| self.rows.iter().filter(|r| r.2 == v).count();
        println!(
            "D5_SUMMARY: {} rows={} MATCH={} MISMATCH={} NOT_SUPPORTED={} NOT_PROVEN={}",
            self.test,
            self.rows.len(),
            count(Verdict::Match),
            count(Verdict::Mismatch),
            count(Verdict::NotSupported),
            count(Verdict::NotProven)
        );
        let unexpected: Vec<String> = self
            .rows
            .iter()
            .filter(|r| r.2 == Verdict::Mismatch)
            .filter(|r| !self.expected.iter().any(|e| e.0 == r.0 && (e.1 == r.1 || e.1 == "*")))
            .map(|r| format!("{} {} ({})", r.0, r.1, r.3))
            .collect();
        assert!(unexpected.is_empty(), "unexpected MISMATCH: {unexpected:#?}");
    }
}

// ---------------------------------------------------------------------------------------------
/// Studio side of one probe: the imported InDesign document, the shaped paragraph and the font's
/// hyphen and space advances.
struct Probe {
    /// Arial ascent at 12 pt from the font's `hhea` table: InDesign's default first baseline
    /// (Ascent offset) and the ascent of the shaped paragraph.
    ascent_pt: f64,
    document: StudioDocument,
    story: StudioTextStory,
    paragraph: ShapedParagraph,
    space_pt: f64,
    hyphen_pt: f64,
}

/// `hhea.ascender / head.unitsPerEm * SIZE` read from the TrueType table directory (bounded).
fn ascent_pt(font: &[u8]) -> Option<f64> {
    let be16 = |at: usize| -> Option<u16> { Some(u16::from_be_bytes([*font.get(at)?, *font.get(at.checked_add(1)?)?])) };
    let be32 = |at: usize| -> Option<u32> { Some(u32::from_be_bytes(font.get(at..at.checked_add(4)?)?.try_into().ok()?)) };
    let tables = usize::from(be16(4)?).min(256);
    let find = |tag: &[u8; 4]| -> Option<usize> {
        (0..tables).find_map(|i| {
            let rec = 12usize.checked_add(i.checked_mul(16)?)?;
            (font.get(rec..rec.checked_add(4)?)? == tag).then(|| be32(rec + 8).map(|o| o as usize)).flatten()
        })
    };
    let units = f64::from(be16(find(b"head")?.checked_add(18)?)?);
    let ascender = f64::from(be16(find(b"hhea")?.checked_add(4)?)? as i16);
    (units > 0.0).then(|| ascender / units * SIZE)
}

fn font(root: &Path) -> Result<FontFile, String> {
    let bytes = read_pinned(root, FONT_DIR, "arial.ttf", FONT_PIN)?;
    let hash = hsk_studio_prism::profile_hash(&bytes);
    Ok(FontFile { identity: "Arial", bytes, hash })
}

fn advance_of(files: &[FontFile], text: &str) -> f64 {
    let segs = [Seg { start: 0, end: text.len() as u64, font: 0, script: *b"Latn", language: "en-US", axes: vec![] }];
    shape(files, text, &segs, false, SIZE, |p| ShapedParagraph::from_type(&p.paragraphs()[0], LEADING, 0.0).glyphs.iter().map(|g| g.advance_pt).sum())
}

fn probe(root: &Path) -> Result<Probe, String> {
    let files = [font(root)?];
    let ascent = ascent_pt(&files[0].bytes).ok_or("arial.ttf has no readable head/hhea")?;
    let idml = oracle(root, "ID-03__para_nohyph_w240.idml")?;
    let import = import_idml(&idml, &Limits::default(), &mut SequentialIdMint::new(1), &CancellationToken::default())
        .map_err(|e| format!("import_idml refused: {e:?}"))?;
    let document = import.proposal.snapshot.document().clone();
    let story = import.proposal.stories.first().ok_or("IDML has no story")?.story.clone();
    match &story.content {
        StoryContent::Inline { text_utf8 } if text_utf8 == TEXT => {}
        other => return Err(format!("imported story text differs from the probe TEXT: {other:?}")),
    }
    let segs = [Seg { start: 0, end: TEXT.len() as u64, font: 0, script: *b"Latn", language: "en-US", axes: vec![] }];
    let paragraph = shape(&files, TEXT, &segs, false, SIZE, |p| ShapedParagraph::from_type(&p.paragraphs()[0], LEADING, ascent));
    Ok(Probe { ascent_pt: ascent, document, story, paragraph, space_pt: advance_of(&files, " "), hyphen_pt: advance_of(&files, "-") })
}

/// One InDesign paragraph setup.
#[derive(Clone, Copy)]
struct Setup {
    paragraph_composer: bool,
    hyphenate: bool,
    justify: bool,
    /// FULLY_JUSTIFIED (ID-02, ID-03): the last line is justified too.
    full: bool,
    width: f64,
    ws: [f64; 3],
    ls: [f64; 3],
    /// Glyph scaling, percent min / desired / max.
    gs: [f64; 3],
}

fn compose(p: &Probe, s: &Setup) -> Result<LayoutProposal, String> {
    compose_in(p, s, p.document.clone(), true)
}

fn compose_in(p: &Probe, s: &Setup, mut doc: StudioDocument, set_width: bool) -> Result<LayoutProposal, String> {
    let mut frames = 0;
    for layer in &mut doc.layers {
        if let Some(f) = layer.text_frame.as_mut() {
            if set_width {
                f.bounds.width = Length { value: s.width, unit: GeometryUnit::Pt };
            }
            f.first_baseline_offset = FirstBaseline::Fixed;
            f.minimum_first_baseline_offset = Length { value: p.ascent_pt, unit: GeometryUnit::Pt };
            frames += 1;
        }
    }
    if set_width && frames != 1 {
        return Err(format!("expected one text frame, found {frames}"));
    }
    let jp = JustifyParams {
        word_spacing: Spacing { min: s.ws[0] / 100.0, desired: s.ws[1] / 100.0, max: s.ws[2] / 100.0 },
        letter_spacing: Spacing { min: s.ls[0] / 100.0, desired: s.ls[1] / 100.0, max: s.ls[2] / 100.0 },
        reference_space_pt: p.space_pt,
        glyph_scaling: Spacing { min: s.gs[0] / 100.0, desired: s.gs[1] / 100.0, max: s.gs[2] / 100.0 },
    };
    // InDesign: hyphenateWordsLongerThan 5 (= at least 5 letters, module docs), after first 2,
    // before last 2, ladder limit 3.
    let hyph = HyphenationParams { min_word_len: 5, min_before: 2, min_after: 2, consecutive_limit: 3, hyphen_advance_pt: p.hyphen_pt, ..HyphenationParams::default() };
    // InDesign's en-US hyphenation where the oracle shows it (lane SMB-ID-COMPOSER-PARITY).
    let opts = TypesetOptions {
        justify: Some(jp),
        hyphenation: s.hyphenate.then_some(hyph),
        hyphenator: Some(Hyphenator::en_us_indesign()),
        alignment: if s.full { Alignment::JustifiedAll } else if s.justify { Alignment::Justified } else { Alignment::Left },
        ..TypesetOptions::default()
    };
    // The InDesign-parity composers (IDML HyphenationZone 36 = the default).
    let composer = if s.paragraph_composer { Composer::AdobeParagraph(AdobeParams::default()) } else { Composer::AdobeSingleLine(AdobeParams::default()) };
    let paragraphs = [p.paragraph.clone()];
    let req = ReflowRequest {
        document: &doc,
        story: &p.story,
        resolved_text: None,
        paragraphs: &paragraphs,
        obstacles: &[],
        composer,
        space: SpaceParams::default(),
        expected_revision: doc.revision,
        limits: ReflowLimits::default(),
    };
    reflow_typeset(&req, &opts, &CancellationToken::default()).map_err(|e| format!("reflow refused {}", e.code()))
}

/// One composed line: source start, source end, `(char index, x)` per glyph.
type StudioLine = (usize, usize, Vec<(usize, f64)>);
/// One ID-02 set: name, word spacing, letter spacing, glyph scaling (percent min/desired/max).
type SpacingSet = (&'static str, [f64; 3], [f64; 3], [f64; 3]);

/// Studio per-character x on each line: glyph advances with the line's justification (spaces times
/// `word_scale`, other glyphs times `glyph_scale`, `letter_gap_pt` after every glyph but the line's
/// last content glyph); a ligature's later components at its carets (module docs); a ligature split
/// by a hyphen set as the line's `start_part` / `end_part`.
fn studio_lines(p: &Probe, placed: &LayoutProposal) -> Vec<StudioLine> {
    let mut out = Vec::new();
    for frame in &placed.frames {
        for line in &frame.lines {
            let (scale, glyph_scale, gap) = line.justification.map_or((1.0, 1.0, 0.0), |j| (j.word_scale, j.glyph_scale, j.letter_gap_pt));
            let mut x = 0.0;
            let mut xs = Vec::new();
            let glyphs = &p.paragraph.glyphs[line.glyph_range.clone()];
            let space = |g: &hsk_studio_interop_layout::layout::LayoutGlyph| TEXT.as_bytes().get(g.cluster.start as usize) == Some(&b' ');
            // A ligature split at the line end sets its left part after the range's glyphs.
            let tail = line.end_part.filter(|e| e.glyph == line.glyph_range.end);
            // `Justification::letter_gap_pt` follows every glyph but the line's last content glyph.
            let last_content = if tail.is_some() { Some(glyphs.len()) } else { glyphs.iter().rposition(|g| !space(g)) };
            for (k, g) in glyphs.iter().enumerate() {
                let gap_after = if last_content.is_some_and(|l| k < l) { gap } else { 0.0 };
                let head = line.start_part.filter(|_| k == 0);
                let (start, end, advance) = match head {
                    Some(h) => (h.source.start, h.source.end, h.advance_pt),
                    None => (g.cluster.start, g.cluster.end, g.advance_pt),
                };
                let w = if space(g) { advance * scale } else { advance * glyph_scale };
                xs.push((start as usize, x));
                // Ligature carets: component c of n at c / n of the scaled advance plus the gap.
                let n = (end - start) as usize;
                for c in 1..n {
                    xs.push((start as usize + c, x + w * c as f64 / n as f64 + gap_after));
                }
                x += w + gap_after;
            }
            if let Some(t) = tail {
                xs.push((t.source.start as usize, x));
            }
            out.push((line.source.start as usize, line.source.end as usize, xs));
        }
    }
    out
}

/// Compares one oracle JSON with Studio's composition of `setup`.
fn compare(p: &Probe, oracle_json: &Value, s: &Setup) -> (Verdict, String) {
    let placed = match compose(p, s) {
        Ok(pl) => pl,
        Err(e) => return (Verdict::Mismatch, e),
    };
    let lines = oracle_json["lines"].as_array().expect("lines");
    let o_starts: Vec<usize> = lines.iter().map(|l| l["start"].as_u64().expect("start") as usize).collect();
    let studio = studio_lines(p, &placed);
    let s_starts: Vec<usize> = studio.iter().map(|l| l.0).collect();
    let first_diff = o_starts.iter().zip(&s_starts).position(|(a, b)| a != b).or((o_starts.len() != s_starts.len()).then(|| o_starts.len().min(s_starts.len())));
    // x on lines whose start and next start agree.
    let chars: std::collections::BTreeMap<usize, f64> = oracle_json["char_x_pt"]
        .as_array()
        .expect("char_x_pt")
        .iter()
        .map(|c| (c[0].as_u64().expect("index") as usize, c[1].as_f64().expect("x")))
        .collect();
    let ff = TEXT.find("ff").expect("probe text holds ff");
    // Every character is held to X_TOL; the ff line's own maximum is still reported.
    let (mut x_max, mut x_ff, mut x_n) = (0.0f64, 0.0f64, 0usize);
    // Worst character (diagnostics): (|dx|, char, Studio x, InDesign x, line, the line's justification).
    let justs: Vec<_> = placed.frames.iter().flat_map(|f| f.lines.iter().map(|l| l.justification)).collect();
    let mut worst = None::<(f64, usize, f64, f64, usize)>;
    for (i, (start, end, xs)) in studio.iter().enumerate() {
        let ff_line = (*start..*end).contains(&(ff + 1));
        let same_start = o_starts.get(i) == Some(start);
        let same_next = o_starts.get(i + 1).copied() == studio.get(i + 1).map(|l| l.0) || (i + 1 == studio.len() && i + 1 == o_starts.len());
        if !(same_start && same_next) {
            continue;
        }
        for (c, x) in xs {
            if let Some(ox) = chars.get(c) {
                let d = (x - ox).abs();
                if worst.is_none_or(|w| d > w.0) {
                    worst = Some((d, *c, *x, *ox, i));
                }
                if ff_line && *c > ff {
                    x_ff = x_ff.max(d);
                } else {
                    x_max = x_max.max(d);
                }
                x_n += 1;
            }
        }
    }
    let overset = placed.overset.is_some();
    let detail = format!(
        "lines ai={} studio={} first_break_diff={:?} x_max_abs_pt={x_max:.4} ff_line_x_max_abs_pt={x_ff:.4} over {x_n} chars overset={overset} oracle_starts={o_starts:?} studio_starts={s_starts:?} worst_char={:?}",
        o_starts.len(),
        s_starts.len(),
        first_diff.map(|i| (i, o_starts.get(i), s_starts.get(i))),
        worst.filter(|w| w.0 > X_TOL).map(|w| (w.1, w.2, w.3, w.4, justs.get(w.4).copied().flatten()))
    );
    let ok = first_diff.is_none() && x_max <= X_TOL && x_ff <= X_TOL && !overset;
    (if ok { Verdict::Match } else { Verdict::Mismatch }, detail)
}

fn load_json(root: &Path, file: &str) -> Result<Value, String> {
    let bytes = oracle(root, file)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{file}: {e}"))
}

fn setup_or_not_proven(test: &'static str, ledger: &mut Ledger) -> Option<(PathBuf, Probe)> {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: {test} reason=oracle-corpus root not found via .config/oracle-corpus.toml");
        return None;
    };
    match probe(&root) {
        Ok(p) => Some((root, p)),
        Err(reason) => {
            ledger.record("ID", "setup", Verdict::NotProven, reason);
            None
        }
    }
}

/// ID-01: 10 widths x {Paragraph, Single-line} composer x hyphenation on/off x {left, left-justified}.
#[test]
fn id01_composer_width_sweep() {
    const T: &str = "d5_id_oracle::id01_composer_width_sweep";
    let mut ledger = Ledger::new(T);
    let Some((root, p)) = setup_or_not_proven(T, &mut ledger) else { return ledger.finish() };
    for (cn, para) in [("para", true), ("single", false)] {
        for (hn, hyph) in [("hyph", true), ("nohyph", false)] {
            for (jn, just) in [("leftjust", true), ("left", false)] {
                for w in (150..=420).step_by(30) {
                    let variant = format!("{cn}_{hn}_{jn}_w{w}");
                    let json = match load_json(&root, &format!("ID-01__{variant}.json")) {
                        Ok(j) => j,
                        Err(e) => {
                            ledger.record("ID-01", &variant, Verdict::NotProven, e);
                            continue;
                        }
                    };
                    let s = Setup { paragraph_composer: para, hyphenate: hyph, justify: just, full: false, width: f64::from(w), ws: [80.0, 100.0, 133.0], ls: [0.0; 3], gs: [100.0; 3] };
                    let (v, d) = compare(&p, &json, &s);
                    ledger.record("ID-01", &variant, v, d);
                }
            }
        }
    }
    ledger.finish();
}

/// ID-02: word/letter spacing limits and glyph scaling at width 240, both composers, hyphenation on,
/// fully justified (InDesign justifies the last line too: `Alignment::JustifiedAll`).
#[test]
fn id02_spacing_limits() {
    const T: &str = "d5_id_oracle::id02_spacing_limits";
    let mut ledger = Ledger::new(T);
    let Some((root, p)) = setup_or_not_proven(T, &mut ledger) else { return ledger.finish() };
    let sets: [SpacingSet; 6] = [
        ("ws100_100_100", [100.0, 100.0, 100.0], [0.0; 3], [100.0; 3]),
        ("ws50_100_200", [50.0, 100.0, 200.0], [0.0; 3], [100.0; 3]),
        ("ws80_100_133_ls-3_0_3", [80.0, 100.0, 133.0], [-3.0, 0.0, 3.0], [100.0; 3]),
        ("ws80_100_133_gs95_100_105", [80.0, 100.0, 133.0], [0.0; 3], [95.0, 100.0, 105.0]),
        ("ws0_100_300", [0.0, 100.0, 300.0], [0.0; 3], [100.0; 3]),
        ("ws100_100_100_ls-5_0_5_gs90_100_110", [100.0, 100.0, 100.0], [-5.0, 0.0, 5.0], [90.0, 100.0, 110.0]),
    ];
    for (cn, para) in [("para", true), ("single", false)] {
        for (name, ws, ls, gs) in sets {
            let variant = format!("{cn}_{name}");
            let json = match load_json(&root, &format!("ID-02__{variant}.json")) {
                Ok(j) => j,
                Err(e) => {
                    ledger.record("ID-02", &variant, Verdict::NotProven, e);
                    continue;
                }
            };
            let s = Setup { paragraph_composer: para, hyphenate: true, justify: true, full: true, width: 240.0, ws, ls, gs };
            let (v, d) = compare(&p, &json, &s);
            ledger.record("ID-02", &variant, v, d);
        }
    }
    ledger.finish();
}

/// ID-03: the three configurations InDesign also exported as IDML and PDF (frame 240 pt, fully
/// justified). The IDML is the composition input of every case (imported in `probe`); the line
/// data is compared like ID-02.
#[test]
fn id03_idml_configurations() {
    const T: &str = "d5_id_oracle::id03_idml_configurations";
    let mut ledger = Ledger::new(T);
    let Some((root, p)) = setup_or_not_proven(T, &mut ledger) else { return ledger.finish() };
    for (variant, para, hyph) in [("para_hyph_w240", true, true), ("single_hyph_w240", false, true), ("para_nohyph_w240", true, false)] {
        match load_json(&root, &format!("ID-03__{variant}.json")) {
            Ok(json) => {
                let s = Setup { paragraph_composer: para, hyphenate: hyph, justify: true, full: true, width: 240.0, ws: [80.0, 100.0, 133.0], ls: [0.0; 3], gs: [100.0; 3] };
                let (v, d) = compare(&p, &json, &s);
                ledger.record("ID-03", variant, v, d);
            }
            Err(e) => ledger.record("ID-03", variant, Verdict::NotProven, e),
        }
        match oracle(&root, &format!("ID-03__{variant}.idml")).map(|b| {
            import_idml(&b, &Limits::default(), &mut SequentialIdMint::new(1), &CancellationToken::default()).map(|i| i.loss.len())
        }) {
            Ok(Ok(losses)) => ledger.record("ID-03", &format!("{variant}.idml_import"), Verdict::Match, format!("import_idml accepted the InDesign package; {losses} typed loss entries")),
            Ok(Err(e)) => ledger.record("ID-03", &format!("{variant}.idml_import"), Verdict::Mismatch, format!("import_idml refused: {e:?}")),
            Err(e) => ledger.record("ID-03", &format!("{variant}.idml_import"), Verdict::NotProven, e),
        }
    }
    ledger.finish();
}

/// ID-04: two threaded 200 x 120 pt frames (the second 150 pt below the first's top), left
/// justified, Paragraph Composer, hyphenation on: the lines of each frame and the overset flags.
/// The second frame is a copy of the imported InDesign frame layer linked as its successor.
#[test]
fn id04_threaded_frames() {
    const T: &str = "d5_id_oracle::id04_threaded_frames";
    let mut ledger = Ledger::new(T);
    let Some((root, p)) = setup_or_not_proven(T, &mut ledger) else { return ledger.finish() };
    let json = match load_json(&root, "ID-04__two_linked_small_frames.json") {
        Ok(j) => j,
        Err(e) => {
            ledger.record("ID-04", "two_linked_small_frames", Verdict::NotProven, e);
            return ledger.finish();
        }
    };
    let mut doc = p.document.clone();
    let index = doc.layers.iter().position(|l| l.text_frame.is_some()).expect("frame layer");
    let mut second = doc.layers[index].clone();
    let mut base = second.layer_id.clone();
    let last = base.pop().expect("id");
    second.layer_id = format!("{base}{}", if last == 'f' { 'e' } else { 'f' });
    let first_id = doc.layers[index].layer_id.clone();
    let pt = |v: f64| Length { value: v, unit: GeometryUnit::Pt };
    {
        let f = doc.layers[index].text_frame.as_mut().expect("frame");
        f.bounds.width = pt(200.0);
        f.bounds.height = pt(120.0);
        f.next_text_frame = Some(second.layer_id.clone());
    }
    {
        let f = second.text_frame.as_mut().expect("frame");
        f.bounds.width = pt(200.0);
        f.bounds.height = pt(120.0);
        f.bounds.y = Length { value: f.bounds.y.value + 150.0, unit: f.bounds.y.unit };
        f.previous_text_frame = Some(first_id);
        f.next_text_frame = None;
    }
    doc.layers.insert(index + 1, second);
    let s = Setup { paragraph_composer: true, hyphenate: true, justify: true, full: false, width: 200.0, ws: [80.0, 100.0, 133.0], ls: [0.0; 3], gs: [100.0; 3] };
    let (verdict, detail) = match compose_in(&p, &s, doc, false) {
        Err(e) => (Verdict::Mismatch, e),
        Ok(placed) => {
            let starts = |frame: &Value| -> Vec<u64> { frame["lines"].as_array().expect("lines").iter().map(|l| l["start"].as_u64().expect("start")).collect() };
            let (o1, o2) = (starts(&json["frame1"]), starts(&json["frame2"]));
            let s1: Vec<u64> = placed.frames.first().map(|f| f.lines.iter().map(|l| l.source.start).collect()).unwrap_or_default();
            let s2: Vec<u64> = placed.frames.get(1).map(|f| f.lines.iter().map(|l| l.source.start).collect()).unwrap_or_default();
            let o_over = json["frame2"]["overflows"].as_bool().expect("overflows");
            let ok = o1 == s1 && o2 == s2 && o_over == placed.overset.is_some();
            (if ok { Verdict::Match } else { Verdict::Mismatch },
             format!("frame1 ai={o1:?} studio={s1:?}; frame2 ai={o2:?} studio={s2:?}; overset ai={o_over} studio={:?}", placed.overset))
        }
    };
    ledger.record("ID-04", "two_linked_small_frames", verdict, detail);
    ledger.finish();
}

/// Type composition v2 on the exact Arial InDesign used (lane SMB-TYPE-FF-LIGATURE): "ff" of
/// "differences" is one glyph, Arial's uniFB00 (1094 units, the cmap presentation form; Arial's GSUB
/// has no Latin ligature), with its two f components (569 units each) for hyphenation splits; no
/// line-end advance is recorded for it (no kerning toward "e"). NOT_PROVEN without the corpus font.
#[test]
fn arial_ff_is_the_presentation_form_ligature() {
    const T: &str = "d5_id_oracle::arial_ff_is_the_presentation_form_ligature";
    let mut ledger = Ledger::new(T);
    let Some((_, p)) = setup_or_not_proven(T, &mut ledger) else { return ledger.finish() };
    let ff = TEXT.find("ff").expect("ff") as u64;
    let unit = SIZE / 2048.0;
    let glyphs = &p.paragraph.glyphs;
    let k = glyphs.iter().position(|g| g.cluster.start == ff).expect("glyph at ff");
    assert_eq!((glyphs[k].cluster.start, glyphs[k].cluster.end), (ff, ff + 2), "one glyph covers ff");
    assert!((glyphs[k].advance_pt - 1094.0 * unit).abs() < 1e-12, "{:?}", glyphs[k]);
    let parts: Vec<_> = p.paragraph.ligature_parts.iter().filter(|c| c.glyph == k).collect();
    assert_eq!(parts.len(), 2);
    for (i, c) in parts.iter().enumerate() {
        assert_eq!((c.source.start, c.source.end), (ff + i as u64, ff + i as u64 + 1));
        assert!((c.advance_pt - 569.0 * unit).abs() < 1e-12, "{c:?}");
    }
    assert!(p.paragraph.ligature_parts.iter().all(|c| c.glyph == k), "no other ligature in the probe text");
    assert!(p.paragraph.line_end_advances.iter().all(|a| a.glyph != k));
    // No other glyph of the paragraph covers more than one character.
    assert!(glyphs.iter().enumerate().all(|(i, g)| i == k || g.cluster.end - g.cluster.start == 1));
    ledger.record("ID", "arial_ff_ligature", Verdict::Match, format!("uniFB00 {} pt, components 2 x {} pt", glyphs[k].advance_pt, 569.0 * unit));
    ledger.finish();
}

/// ID-03 through the document model (lane SMB-LAYOUT-COMPOSER-WIRING): each InDesign-written IDML's
/// paragraph composition (`[No paragraph style]` attributes inherited through
/// `NormalParagraphStyle`, local `Composer="HL Single"` / `Hyphenation="false"`) is imported into
/// Folio type-style records with `import_type_styles` and composed from those records alone: the
/// request composer is FirstFit and the options carry no justification limits, no hyphenation and
/// no composer, only the styles, the hyphenator, the paragraph alignment (`Justification` is not
/// mapped yet) and the shaped hyphen advance. The lines must equal the explicit-setup composition
/// that `id03_idml_configurations` compares with InDesign, so the oracle verdicts pinned there
/// carry over to the wired path. NOT_PROVEN without the corpus font.
#[test]
fn id03_imported_styles_compose_as_the_explicit_setup() {
    const T: &str = "d5_id_oracle::id03_imported_styles_compose_as_the_explicit_setup";
    let mut ledger = Ledger::new(T);
    let Some((root, p)) = setup_or_not_proven(T, &mut ledger) else { return ledger.finish() };
    let cancel = CancellationToken::default();
    for (variant, para, hyph) in [("para_hyph_w240", true, true), ("single_hyph_w240", false, true), ("para_nohyph_w240", true, false)] {
        let bytes = match oracle(&root, &format!("ID-03__{variant}.idml")) {
            Ok(b) => b,
            Err(e) => {
                ledger.record("ID-03", &format!("{variant}.styles"), Verdict::NotProven, e);
                continue;
            }
        };
        let mut mint = SequentialIdMint::new(1);
        let import = import_idml(&bytes, &Limits::default(), &mut mint, &cancel).expect("import");
        let styles = import_type_styles(&import, &Limits::default(), &mut mint, &cancel).expect("type styles");
        assert!(!styles.records.style_composition.is_empty(), "{variant}: the root style's composition is mapped");
        let story = import.proposal.stories.first().expect("story").story.clone();
        assert_eq!(story.inline_text(), Some(TEXT), "{variant}");
        let mut doc = import.proposal.snapshot.document().clone();
        for f in doc.layers.iter_mut().filter_map(|l| l.text_frame.as_mut()) {
            f.bounds.width = Length { value: 240.0, unit: GeometryUnit::Pt };
            f.first_baseline_offset = FirstBaseline::Fixed;
            f.minimum_first_baseline_offset = Length { value: p.ascent_pt, unit: GeometryUnit::Pt };
        }
        let opts = TypesetOptions {
            hyphenator: Some(Hyphenator::en_us_indesign()),
            alignment: Alignment::JustifiedAll,
            styles: Some(StyleSource { records: &styles.records, default_font_size_pt: SIZE }),
            hyphen_advance_pt: Some(p.hyphen_pt),
            ..TypesetOptions::default()
        };
        let paragraphs = [p.paragraph.clone()];
        let req = ReflowRequest {
            document: &doc,
            story: &story,
            resolved_text: None,
            paragraphs: &paragraphs,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: ReflowLimits::default(),
        };
        let wired = reflow_typeset(&req, &opts, &cancel).expect("wired composition");
        let s = Setup { paragraph_composer: para, hyphenate: hyph, justify: true, full: true, width: 240.0, ws: [80.0, 100.0, 133.0], ls: [0.0; 3], gs: [100.0; 3] };
        let explicit = compose(&p, &s).expect("explicit composition");
        let lines = |pl: &LayoutProposal| pl.frames.iter().map(|f| f.lines.clone()).collect::<Vec<_>>();
        assert_eq!(lines(&wired), lines(&explicit), "{variant}: records-driven composition differs from the explicit setup");
        println!("D5_WIRING: ID-03 {variant} records-driven lines == explicit setup ({} lines)", wired.frames.iter().map(|f| f.lines.len()).sum::<usize>());
    }
    ledger.finish();
}
