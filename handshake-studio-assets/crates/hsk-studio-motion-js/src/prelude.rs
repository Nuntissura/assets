//! After Effects expression library prelude (lane SMB-AE-EXPR-LIBRARY-1; STU-MOT-073..076, 083, 255).
//! The JavaScript profile binds what Motion's identifier catalogue (`hsk_studio_motion::expr_identifiers`)
//! marks bound and refuses every other member with a typed error; the lists below are derived from
//! that catalogue, never maintained by hand.
//!
//! * Bound host functions (installed as configurable, writable globals, both declared spellings on
//!   one function object): `linear` (D5 oracle AE-03, unchanged), the Vector Math functions `add`,
//!   `mul`, `clamp`, `dot`, `cross`, `normalize`, `length`, the Other Math and Color Conversion
//!   spellings. Arguments are finite numbers or non-empty arrays of finite numbers of the shapes the
//!   definitions need; anything else (wrong arity or shape, mismatched lengths, a zero vector to
//!   `normalize`, `clamp` limits in the wrong order, a non-finite result, a hostile `hexToRgb` text)
//!   is a thrown refusal: `Unsupported`. Its throw site is the prelude line, so it carries no span.
//! * `localeCompare` is deterministic UTF-16 code-unit lexical order (STU-MOT-255); any non-undefined
//!   locale or options argument is refused. The other ES2018 members the catalogue binds are the
//!   realm's own intrinsics.
//! * Every unavailable member that is a plain identifier is a global accessor whose getter is the
//!   native `Number.prototype.valueOf`: reading it throws a native `TypeError` from the user's own
//!   frame, so the refusal is `Unsupported` at the user's line and column. Its setter turns the name
//!   into an ordinary data property, and a name the user declares (`var`, `let`, `const`,
//!   `function`) stays the user's: locals are never captured.
//! * `match`, `search`, `toLocaleLowerCase` and `toLocaleUpperCase` (no regexp compiler, no locale in
//!   the profile) are replaced on `String.prototype` by the same native refusal. `Date` keeps the
//!   realm's absence of a Date binding (`ReferenceMissing`; the STU-MOT-256/257 Date profile is not
//!   part of this lane). `.index` and `.name` are members of host objects, none of which exists.
//!
//! Everything the prelude uses is captured from intrinsics reached through literals and prototypes
//! before any user statement runs.
use hsk_studio_motion::{ExprBinding, ExprProvenance, expr_identifiers};
use std::sync::OnceLock;

/// String members refused by the profile although the realm has them (Unavailable in the catalogue).
pub(crate) const REFUSED_MEMBERS: [&str; 4] = ["match", "search", "toLocaleLowerCase", "toLocaleUpperCase"];

/// Unavailable members that get no global accessor: `Date` stays absent from the realm.
const ABSENT: [&str; 1] = ["Date"];

/// The one-line prelude function. `with_ae_globals` calls it with the catalogue-derived lists
/// `(globalThis, bound, unavailable, refused)`.
pub const AE_GLOBALS_PRELUDE: &str = concat!(
    "(function (g, bound, unavailable, refused) { ",
    "var O = ({}).constructor, def = O.defineProperty, refuse = (0).valueOf, fcall = refuse.call, uncurry = fcall.bind.bind(fcall); ",
    "var has = uncurry(O.prototype.hasOwnProperty), fail = uncurry(refuse), code = uncurry(\"\".charCodeAt), Str = \"\".constructor, isArray = [].constructor.isArray, sqrt = Math.sqrt, PI = 3.141592653589793; ",
    "function bad() { fail(null); } ",
    "function num(x) { if (typeof x !== \"number\" || x - x !== 0) { bad(); } return x; } ",
    "function vec(v) { if (!isArray(v)) { bad(); } var n = v.length, o = []; if (n < 1) { bad(); } for (var i = 0; i < n; i++) { o[i] = num(v[i]); } return o; } ",
    "function out(o) { for (var i = 0; i < o.length; i++) { num(o[i]); } return o; } ",
    "function arity(a, n) { if (a.length !== n) { bad(); } } ",
    "function same(a, b) { if (a.length !== b.length) { bad(); } } ",
    "function norm(x) { var s = 0; for (var i = 0; i < x.length; i++) { s += x[i] * x[i]; } return num(sqrt(s)); } ",
    "function cl(x, a, b) { if (a > b) { bad(); } return x < a ? a : (x > b ? b : x); } ",
    "function unit4(c) { var x = vec(c); if (x.length !== 4) { bad(); } for (var i = 0; i < 4; i++) { if (x[i] < 0 || x[i] > 1) { bad(); } } return x; } ",
    "function hue(p, q, t) { if (t < 0) { t += 1; } if (t > 1) { t -= 1; } if (t < 1 / 6) { return p + (q - p) * 6 * t; } if (t < 1 / 2) { return q; } if (t < 2 / 3) { return p + (q - p) * (2 / 3 - t) * 6; } return p; } ",
    "function hx(c) { if (c >= 48 && c <= 57) { return c - 48; } if (c >= 65 && c <= 70) { return c - 55; } if (c >= 97 && c <= 102) { return c - 87; } bad(); } ",
    "var api = { ",
    "linear: function (t, a, b, c, d) { if (c === undefined) { c = a; d = b; a = 0; b = 1; } if (t <= a) { return c; } if (t >= b) { return d; } var f = (t - a) / (b - a); if (Array.isArray(c)) { var o = []; for (var i = 0; i < c.length; i++) { o.push(c[i] + (d[i] - c[i]) * f); } return o; } return c + (d - c) * f; }, ",
    "add: function (a, b) { arity(arguments, 2); if (typeof a === \"number\") { return num(num(a) + num(b)); } var x = vec(a), y = vec(b); same(x, y); for (var i = 0; i < x.length; i++) { x[i] = x[i] + y[i]; } return out(x); }, ",
    "mul: function (v, k) { arity(arguments, 2); k = num(k); if (typeof v === \"number\") { return num(num(v) * k); } var x = vec(v); for (var i = 0; i < x.length; i++) { x[i] = x[i] * k; } return out(x); }, ",
    "clamp: function (v, l1, l2) { arity(arguments, 3); if (typeof v === \"number\") { return cl(num(v), num(l1), num(l2)); } var x = vec(v), a = vec(l1), b = vec(l2); same(x, a); same(x, b); for (var i = 0; i < x.length; i++) { x[i] = cl(x[i], a[i], b[i]); } return x; }, ",
    "dot: function (a, b) { arity(arguments, 2); var x = vec(a), y = vec(b), s = 0; same(x, y); for (var i = 0; i < x.length; i++) { s += x[i] * y[i]; } return num(s); }, ",
    "cross: function (a, b) { arity(arguments, 2); var x = vec(a), y = vec(b); if (x.length !== 3 || y.length !== 3) { bad(); } return out([x[1] * y[2] - x[2] * y[1], x[2] * y[0] - x[0] * y[2], x[0] * y[1] - x[1] * y[0]]); }, ",
    "normalize: function (v) { arity(arguments, 1); var x = vec(v), n = norm(x); if (n === 0) { bad(); } for (var i = 0; i < x.length; i++) { x[i] = x[i] / n; } return out(x); }, ",
    "length: function (a, b) { if (arguments.length === 1) { return norm(vec(a)); } arity(arguments, 2); var x = vec(a), y = vec(b); same(x, y); for (var i = 0; i < x.length; i++) { x[i] = x[i] - y[i]; } return norm(out(x)); }, ",
    "degreesToRadians: function (d) { arity(arguments, 1); return num(num(d) * PI / 180); }, ",
    "radiansToDegrees: function (r) { arity(arguments, 1); return num(num(r) * 180 / PI); }, ",
    "rgbToHsl: function (c) { arity(arguments, 1); var x = unit4(c), r = x[0], gr = x[1], b = x[2], mx = r, mn = r; if (gr > mx) { mx = gr; } if (b > mx) { mx = b; } if (gr < mn) { mn = gr; } if (b < mn) { mn = b; } var l = (mx + mn) / 2, d = mx - mn, h = 0, s = 0; if (d !== 0) { s = l > 0.5 ? d / (2 - mx - mn) : d / (mx + mn); if (mx === r) { h = (gr - b) / d + (gr < b ? 6 : 0); } else if (mx === gr) { h = (b - r) / d + 2; } else { h = (r - gr) / d + 4; } h = h / 6; } return [h, s, l, x[3]]; }, ",
    "hslToRgb: function (c) { arity(arguments, 1); var x = unit4(c), h = x[0], s = x[1], l = x[2]; if (s === 0) { return [l, l, l, x[3]]; } var q = l < 0.5 ? l * (1 + s) : l + s - l * s, p = 2 * l - q; return [hue(p, q, h + 1 / 3), hue(p, q, h), hue(p, q, h - 1 / 3), x[3]]; }, ",
    "hexToRgb: function (s) { arity(arguments, 1); if (typeof s !== \"string\") { bad(); } var n = s.length, i = 0; if (n === 7 && code(s, 0) === 35) { i = 1; } else if (n !== 6) { bad(); } var o = [0, 0, 0, 1]; for (var k = 0; k < 3; k++) { o[k] = (hx(code(s, i + 2 * k)) * 16 + hx(code(s, i + 2 * k + 1))) / 255; } return o; } ",
    "}; ",
    "function lc(that) { if (this === undefined || this === null) { bad(); } for (var j = 1; j < arguments.length; j++) { if (arguments[j] !== undefined) { bad(); } } var a = Str(this), b = Str(that), n = a.length < b.length ? a.length : b.length; for (var i = 0; i < n; i++) { var x = code(a, i), y = code(b, i); if (x !== y) { return x < y ? -1 : 1; } } return a.length === b.length ? 0 : (a.length < b.length ? -1 : 1); } ",
    "var sp = Str.prototype, i; ",
    "def(sp, \"localeCompare\", { value: lc, writable: true, enumerable: false, configurable: true }); ",
    "for (i = 0; i < refused.length; i++) { def(sp, refused[i], { value: refuse, writable: true, enumerable: false, configurable: true }); } ",
    "for (i = 0; i < bound.length; i++) { var f = api[bound[i][1]]; if (typeof f !== \"function\") { bad(); } if (!has(g, bound[i][0])) { def(g, bound[i][0], { value: f, writable: true, enumerable: false, configurable: true }); } } ",
    "function setter(k) { return function (v) { def(g, k, { value: v, writable: true, enumerable: true, configurable: true }); }; } ",
    "for (i = 0; i < unavailable.length; i++) { var u = unavailable[i]; if (!has(g, u)) { def(g, u, { get: refuse, set: setter(u), enumerable: false, configurable: true }); } } ",
    "})"
);

fn plain_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// Catalogue-derived lists: bound host functions as `(spelling, canonical)` (bound, not an ES2018
/// intrinsic, not the `time` value), unavailable global accessors, refused string members.
pub(crate) struct PreludeLists {
    pub(crate) bound: Vec<(&'static str, &'static str)>,
    pub(crate) unavailable: Vec<&'static str>,
}

pub(crate) fn lists() -> PreludeLists {
    let mut bound = Vec::new();
    let mut unavailable = Vec::new();
    for entry in expr_identifiers() {
        match entry.binding {
            ExprBinding::Bound(ExprProvenance::Spec(_)) => {}
            ExprBinding::Bound(_) if entry.name == "time" => {}
            ExprBinding::Bound(_) => bound.push((entry.name, entry.canonical)),
            ExprBinding::Unavailable => {
                if plain_identifier(entry.name)
                    && !ABSENT.contains(&entry.name)
                    && !REFUSED_MEMBERS.contains(&entry.name)
                {
                    unavailable.push(entry.name);
                }
            }
        }
    }
    PreludeLists { bound, unavailable }
}

/// The call that follows [`AE_GLOBALS_PRELUDE`] on the same line (built once).
pub(crate) fn prelude_call() -> &'static str {
    static CALL: OnceLock<String> = OnceLock::new();
    CALL.get_or_init(|| {
        let lists = lists();
        let quote = |s: &str| format!("\"{s}\"");
        let bound: Vec<String> = lists.bound.iter().map(|(s, c)| format!("[{}, {}]", quote(s), quote(c))).collect();
        let unavailable: Vec<String> = lists.unavailable.iter().map(|s| quote(s)).collect();
        let refused: Vec<String> = REFUSED_MEMBERS.iter().map(|s| quote(s)).collect();
        format!(
            "(globalThis, [{}], [{}], [{}]);",
            bound.join(", "),
            unavailable.join(", "),
            refused.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsk_studio_motion::expr_binding;

    #[test]
    fn prelude_lists_follow_the_catalogue() {
        let lists = lists();
        assert_eq!(lists.bound.len(), 17);
        assert!(lists.bound.contains(&("linear", "linear")));
        assert!(lists.bound.contains(&("degrees_to_radians", "degreesToRadians")));
        assert!(lists.bound.contains(&("hexToRgb", "hexToRgb")));
        assert!(!lists.bound.iter().any(|(s, _)| *s == "time" || *s == "sin" || *s == "lookAt"));
        // 283 unavailable minus `.index`, `.name`, `Date` and the four refused string members.
        assert_eq!(lists.unavailable.len(), 276);
        for name in REFUSED_MEMBERS.iter().chain(&ABSENT) {
            assert!(expr_binding(name).is_err(), "{name} must be unavailable in the catalogue");
            assert!(!lists.unavailable.contains(name));
        }
        assert!(lists.unavailable.contains(&"wiggle") && lists.unavailable.contains(&"BEEp_EvalExprEngines"));
        assert!(lists.unavailable.iter().all(|n| plain_identifier(n)));
    }

    /// The prelude and its call stay on ONE line (`user_span` moves spans up by exactly one line) and
    /// carry only plain identifiers inside the quoted lists.
    #[test]
    fn prelude_is_one_line() {
        for text in [AE_GLOBALS_PRELUDE, prelude_call()] {
            assert!(!text.contains(['\n', '\r', '\u{2028}', '\u{2029}']));
        }
        assert!(prelude_call().starts_with("(globalThis, [[\"linear\", \"linear\"]") || prelude_call().contains("[\"linear\", \"linear\"]"));
        assert!(prelude_call().ends_with("[\"match\", \"search\", \"toLocaleLowerCase\", \"toLocaleUpperCase\"]);"));
    }
}
