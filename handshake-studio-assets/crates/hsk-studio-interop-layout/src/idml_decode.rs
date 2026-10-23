//! Safe IDML package open and the bounded semantic decode of the supported subset: designmap ->
//! spreads (pages, text frames, thread links) and stories (paragraph and character ranges with
//! applied style references). XML is read with roxmltree: DTDs (hence entity declarations) are
//! refused, no external reference is ever followed, the node count is bounded. Everything outside
//! the subset stays verbatim in the package (and in `Opaque` children of the model) and is listed in
//! the typed loss report.
#![deny(clippy::arithmetic_side_effects)]
use crate::container;
use crate::ids::IdMint;
use crate::model::*;
use crate::{IdmlError, Result, proposal};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{AutoSizing, FirstBaseline, VerticalJustification};
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// `(spread index, frame index)`.
pub(crate) type Pos = (usize, usize);
/// Resolved `(previous, next)` of a frame.
pub(crate) type Links = (Option<Pos>, Option<Pos>);
pub(crate) const IDPKG_NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const DESIGNMAP: &str = "designmap.xml";

/// Loss entries keyed and ordered deterministically; repeated notes only raise the count.
#[derive(Default)]
pub(crate) struct LossBook(BTreeMap<(String, String, LossKind, &'static str), u32>);
impl LossBook {
    pub(crate) fn note(&mut self, part: &str, element: &str, kind: LossKind, code: &'static str) {
        let count = self.0.entry((part.to_owned(), element.to_owned(), kind, code)).or_insert(0);
        *count = count.saturating_add(1);
    }
    pub(crate) fn finish(self) -> Vec<LossEntry> {
        self.0
            .into_iter()
            .map(|((part, element, kind, code), count)| LossEntry {
                part,
                element,
                kind,
                code,
                count,
            })
            .collect()
    }
}

pub(crate) fn canceled(cancel: &CancellationToken) -> Result<()> {
    cancel.check().map_err(|_| IdmlError::Canceled)
}

fn map_xml(e: roxmltree::Error) -> IdmlError {
    use roxmltree::Error as E;
    match e {
        E::DtdDetected => IdmlError::EntityDenied,
        E::NodesLimitReached | E::AttributesLimitReached | E::NamespacesLimitReached => {
            IdmlError::BudgetExceeded("xml_nodes")
        }
        _ => IdmlError::MalformedXml,
    }
}

pub(crate) fn text_of(part: &IdmlPart) -> Result<&str> {
    std::str::from_utf8(&part.bytes).map_err(|_| IdmlError::MalformedXml)
}

pub(crate) fn parse_xml<'a>(text: &'a str, limits: &Limits) -> Result<Document<'a>> {
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: limits.xml_nodes,
        ..ParsingOptions::default()
    };
    Document::parse_with_options(text, options).map_err(map_xml)
}

fn name<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}
fn elements<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}
fn child<'a, 'i>(n: Node<'a, 'i>, wanted: &str) -> Option<Node<'a, 'i>> {
    elements(n).find(|c| name(*c) == wanted)
}
fn attr<'a>(n: Node<'a, '_>, wanted: &str) -> Option<&'a str> {
    n.attributes()
        .find(|a| a.name() == wanted && a.namespace().is_none())
        .map(|a| a.value())
}
fn extras(n: Node, handled: &[&str]) -> Attrs {
    n.attributes()
        .filter(|a| a.namespace().is_none() && !handled.contains(&a.name()))
        .map(|a| (a.name().to_owned(), a.value().to_owned()))
        .collect()
}
fn opaque(n: Node, text: &str) -> Opaque {
    Opaque {
        element: name(n).to_owned(),
        xml: text[n.range()].to_owned(),
    }
}
/// Fixed bytes charged per paragraph / run on top of the shared range data and the run text: the
/// tags canonical export writes around them plus the size of the model record itself.
const PARA_OVERHEAD: u64 = 128;
const RUN_OVERHEAD: u64 = 192;

/// What canonical export writes for one use of a range's shared data: style name, attributes and
/// verbatim `Properties` XML.
fn range_weight(style: &str, attrs: &Attrs, props: Option<&Opaque>) -> u64 {
    let attrs = attrs.iter().fold(0u64, |sum, (k, v)| sum.saturating_add(k.len() as u64).saturating_add(v.len() as u64).saturating_add(4));
    (style.len() as u64).saturating_add(attrs).saturating_add(props.map_or(0, |p| p.xml.len() as u64))
}
fn link(v: Option<&str>) -> Option<String> {
    v.filter(|s| !s.is_empty() && *s != "n").map(str::to_owned)
}
fn parse_f(s: &str) -> Result<f64> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() <= 1e9)
        .ok_or(IdmlError::InvalidGeometry)
}
fn floats(s: &str, n: usize) -> Result<Vec<f64>> {
    let v = s
        .split_whitespace()
        .map(parse_f)
        .collect::<Result<Vec<_>>>()?;
    if v.len() == n {
        Ok(v)
    } else {
        Err(IdmlError::InvalidGeometry)
    }
}
fn affine(n: Node) -> Result<Affine> {
    match attr(n, "ItemTransform") {
        None => Ok(IDENTITY),
        Some(s) => {
            let v = floats(s, 6)?;
            Ok([v[0], v[1], v[2], v[3], v[4], v[5]])
        }
    }
}
/// The element named `wanted`, directly or inside the `idPkg:` wrapper element of the same name.
fn inner<'a, 'i>(root: Node<'a, 'i>, wanted: &str) -> Result<Node<'a, 'i>> {
    if name(root) != wanted {
        return Err(IdmlError::NotIdml("part_root"));
    }
    if root.tag_name().namespace() == Some(IDPKG_NS) {
        child(root, wanted).ok_or(IdmlError::NotIdml("wrapper_empty"))
    } else {
        Ok(root)
    }
}

/// Opens the container safely (see [`crate::container`]) and keeps every part verbatim.
pub fn open_idml(bytes: &[u8], limits: &Limits, cancel: &CancellationToken) -> Result<IdmlPackage> {
    let parts: Vec<IdmlPart> = container::read(bytes, limits, cancel)?
        .into_iter()
        .map(|(name, data)| IdmlPart::new(name, data))
        .collect();
    if !parts.iter().any(|p| p.name == DESIGNMAP) {
        return Err(IdmlError::NotIdml("designmap_missing"));
    }
    Ok(IdmlPackage { parts })
}

/// `open_idml` followed by `decode_idml`.
pub fn import_idml(
    bytes: &[u8],
    limits: &Limits,
    mint: &mut dyn IdMint,
    cancel: &CancellationToken,
) -> Result<IdmlImport> {
    decode_idml(open_idml(bytes, limits, cancel)?, limits, mint, cancel)
}

/// Decodes the supported subset into the typed model and the Folio proposal, with the loss report.
pub fn decode_idml(
    package: IdmlPackage,
    limits: &Limits,
    mint: &mut dyn IdMint,
    cancel: &CancellationToken,
) -> Result<IdmlImport> {
    canceled(cancel)?;
    let mut loss = LossBook::default();
    let model = read_model(&package, limits, &mut loss, cancel)?;
    let proposal = proposal::project(&model, limits, mint, cancel, &mut loss)?;
    Ok(IdmlImport {
        package,
        model,
        proposal,
        loss: loss.finish(),
    })
}

fn read_model(
    package: &IdmlPackage,
    limits: &Limits,
    loss: &mut LossBook,
    cancel: &CancellationToken,
) -> Result<IdmlModel> {
    let map_part = package
        .part(DESIGNMAP)
        .ok_or(IdmlError::NotIdml("designmap_missing"))?;
    let map_text = text_of(map_part)?;
    let map_doc = parse_xml(map_text, limits)?;
    let root = map_doc.root_element();
    if name(root) != "Document" {
        return Err(IdmlError::NotIdml("designmap_root"));
    }
    let mut designmap = Vec::new();
    let (mut spread_parts, mut story_parts, mut pref_parts) = (Vec::new(), Vec::new(), Vec::new());
    let mut handled: BTreeSet<&str> = BTreeSet::from(["mimetype", DESIGNMAP]);
    for c in elements(root) {
        canceled(cancel)?;
        let idpkg = c.tag_name().namespace() == Some(IDPKG_NS);
        let src = idpkg.then(|| attr(c, "src")).flatten();
        if let Some(src) = src {
            if package.part(src).is_none() {
                return Err(IdmlError::DanglingPart);
            }
            match name(c) {
                "Spread" | "Story" if !handled.insert(src) => {
                    return Err(IdmlError::NotIdml("duplicate_part_reference"));
                }
                "Spread" => {
                    designmap.push(DesignmapItem::Spread(spread_parts.len()));
                    spread_parts.push(src);
                    continue;
                }
                "Story" => {
                    designmap.push(DesignmapItem::Story(story_parts.len()));
                    story_parts.push(src);
                    continue;
                }
                "Preferences" => pref_parts.push(src),
                _ => {}
            }
        }
        loss.note(
            DESIGNMAP,
            name(c),
            LossKind::PreservedOpaque,
            "designmap_element_not_interpreted",
        );
        designmap.push(DesignmapItem::Opaque(opaque(c, map_text)));
    }
    for p in &package.parts {
        if !handled.contains(p.name.as_str()) {
            loss.note(
                &p.name,
                "part",
                LossKind::PreservedOpaque,
                "part_preserved_verbatim",
            );
        }
    }

    let mut document = DocumentSettings::default();
    for src in pref_parts {
        let part = package.part(src).ok_or(IdmlError::DanglingPart)?;
        let doc = parse_xml(text_of(part)?, limits)?;
        for e in elements(doc.root_element()) {
            read_preference(e, &mut document)?;
        }
    }
    for e in elements(root) {
        read_preference(e, &mut document)?;
    }

    let mut spreads = Vec::new();
    for src in spread_parts {
        canceled(cancel)?;
        let part = package.part(src).ok_or(IdmlError::DanglingPart)?;
        spreads.push(read_spread(part, limits, loss, cancel)?);
    }
    let mut stories = Vec::new();
    let mut charged = 0u64;
    for src in story_parts {
        canceled(cancel)?;
        let part = package.part(src).ok_or(IdmlError::DanglingPart)?;
        stories.push(read_story(part, limits, loss, &mut charged, cancel)?);
    }
    Ok(IdmlModel {
        dom_version: attr(root, "DOMVersion").unwrap_or_default().to_owned(),
        document_attrs: root_attrs(root),
        designmap,
        document,
        spreads,
        stories,
    })
}

/// Document attributes except the regenerated `StoryList` / `DOMVersion`, plus namespace
/// declarations other than `idPkg` (verbatim designmap children may use their prefixes).
fn root_attrs(root: Node) -> Attrs {
    let mut a = extras(root, &["StoryList", "DOMVersion"]);
    for ns in root.namespaces() {
        if let Some(prefix) = ns.name().filter(|p| *p != "idPkg" && *p != "xml") {
            a.push((format!("xmlns:{prefix}"), ns.uri().to_owned()));
        }
    }
    a
}

fn read_preference(e: Node, document: &mut DocumentSettings) -> Result<()> {
    let opt = |k: &str| attr(e, k).map(parse_f).transpose();
    match name(e) {
        "DocumentPreference" => {
            document.page_width_pt = opt("PageWidth")?;
            document.page_height_pt = opt("PageHeight")?;
            document.facing_pages = attr(e, "FacingPages") == Some("true");
            let keys = [
                "DocumentBleedTopOffset",
                "DocumentBleedBottomOffset",
                "DocumentBleedInsideOrLeftOffset",
                "DocumentBleedOutsideOrRightOffset",
            ];
            if keys.iter().any(|k| attr(e, k).is_some()) {
                let mut bleed = [0.0; 4];
                for (slot, k) in bleed.iter_mut().zip(keys) {
                    *slot = opt(k)?.unwrap_or(0.0);
                }
                document.bleed_pt = Some(bleed);
            }
        }
        "MarginPreference" => document.margins_pt = Some(margins_of(e)?),
        _ => {}
    }
    Ok(())
}

fn margins_of(e: Node) -> Result<[f64; 4]> {
    let mut m = [0.0; 4];
    for (slot, k) in m.iter_mut().zip(["Top", "Bottom", "Left", "Right"]) {
        *slot = attr(e, k).map(parse_f).transpose()?.unwrap_or(0.0);
    }
    Ok(m)
}

fn page_item_code(element: &str) -> &'static str {
    match element {
        "Rectangle" | "Oval" | "Polygon" | "GraphicLine" => "graphic_page_item_not_interpreted",
        "Group" => "group_not_descended",
        "Guide" => "guide_not_interpreted",
        _ => "spread_element_not_interpreted",
    }
}

fn read_spread(
    part: &IdmlPart,
    limits: &Limits,
    loss: &mut LossBook,
    cancel: &CancellationToken,
) -> Result<SpreadModel> {
    let text = text_of(part)?;
    let doc = parse_xml(text, limits)?;
    let spread = inner(doc.root_element(), "Spread")?;
    let mut m = SpreadModel {
        id: attr(spread, "Self")
            .ok_or(IdmlError::NotIdml("spread_self"))?
            .to_owned(),
        part: part.name.clone(),
        item_transform: affine(spread)?,
        attrs: extras(spread, &["Self", "PageCount", "ItemTransform"]),
        pages: Vec::new(),
        frames: Vec::new(),
        items: Vec::new(),
    };
    for c in elements(spread) {
        canceled(cancel)?;
        match name(c) {
            "Page" => {
                m.items.push(SpreadItem::Page(m.pages.len()));
                m.pages.push(read_page(c, text, &part.name, loss)?);
            }
            "TextFrame" => {
                m.items.push(SpreadItem::Frame(m.frames.len()));
                m.frames.push(read_frame(c, text, &part.name, loss)?);
            }
            other => {
                loss.note(
                    &part.name,
                    other,
                    LossKind::PreservedOpaque,
                    page_item_code(other),
                );
                // Frames inside groups (at any depth) are not decoded: their group transforms are
                // not composed. Each one is counted so a story that flows through them is never
                // silently shorter than the source; the group itself stays verbatim.
                for _ in c
                    .descendants()
                    .filter(|d| d.is_element() && name(*d) == "TextFrame")
                {
                    loss.note(
                        &part.name,
                        "TextFrame",
                        LossKind::PreservedOpaque,
                        "text_frame_in_group_not_decoded",
                    );
                }
                m.items.push(SpreadItem::Opaque(opaque(c, text)));
            }
        }
    }
    Ok(m)
}

fn read_page(n: Node, text: &str, part: &str, loss: &mut LossBook) -> Result<PageModel> {
    let b = floats(
        attr(n, "GeometricBounds").ok_or(IdmlError::InvalidGeometry)?,
        4,
    )?;
    if b[2] <= b[0] || b[3] <= b[1] {
        return Err(IdmlError::InvalidGeometry);
    }
    let mut margins_pt = None;
    let mut children = Vec::new();
    for c in elements(n) {
        if name(c) == "MarginPreference" {
            margins_pt = Some(margins_of(c)?);
        } else {
            loss.note(
                part,
                name(c),
                LossKind::PreservedOpaque,
                "page_element_not_interpreted",
            );
        }
        children.push(opaque(c, text));
    }
    Ok(PageModel {
        id: attr(n, "Self")
            .ok_or(IdmlError::NotIdml("page_self"))?
            .to_owned(),
        name: attr(n, "Name").unwrap_or_default().to_owned(),
        bounds_pt: [b[0], b[1], b[2], b[3]],
        item_transform: affine(n)?,
        attrs: extras(n, &["Self", "Name", "GeometricBounds", "ItemTransform"]),
        children,
        margins_pt,
    })
}

fn read_frame(n: Node, text: &str, part: &str, loss: &mut LossBook) -> Result<FrameModel> {
    let (path, path_open) = read_path(n, part, loss)?;
    if path.len() < 3 {
        return Err(IdmlError::InvalidGeometry);
    }
    let mut children = Vec::new();
    for c in elements(n) {
        match name(c) {
            "Properties" => {
                for p in elements(c).filter(|p| name(*p) != "PathGeometry") {
                    loss.note(
                        part,
                        name(p),
                        LossKind::PreservedOpaque,
                        "frame_property_not_interpreted",
                    );
                }
            }
            "TextFramePreference" => {}
            "TextWrapPreference" => {
                loss.note(
                    part,
                    "TextWrapPreference",
                    LossKind::PreservedOpaque,
                    "text_wrap_not_interpreted",
                );
            }
            other => {
                loss.note(
                    part,
                    other,
                    LossKind::PreservedOpaque,
                    "frame_element_not_interpreted",
                );
            }
        }
        children.push(opaque(c, text));
    }
    let attrs = extras(
        n,
        &[
            "Self",
            "ParentStory",
            "PreviousTextFrame",
            "NextTextFrame",
            "ItemTransform",
        ],
    );
    if !attrs.is_empty() {
        loss.note(
            part,
            "TextFrame",
            LossKind::PreservedOpaque,
            "frame_attributes_not_interpreted",
        );
    }
    Ok(FrameModel {
        id: attr(n, "Self")
            .ok_or(IdmlError::NotIdml("frame_self"))?
            .to_owned(),
        story_id: link(attr(n, "ParentStory")).ok_or(IdmlError::NotIdml("frame_story"))?,
        previous: link(attr(n, "PreviousTextFrame")),
        next: link(attr(n, "NextTextFrame")),
        item_transform: affine(n)?,
        attrs,
        children,
        path,
        path_open,
        prefs: read_prefs(n, part, loss)?,
    })
}

fn read_path(frame: Node, part: &str, loss: &mut LossBook) -> Result<(Vec<PathPoint>, bool)> {
    let geometry = child(frame, "Properties").and_then(|p| child(p, "PathGeometry"));
    let Some(geometry) = geometry else {
        return Ok((Vec::new(), false));
    };
    let subpaths: Vec<Node> = elements(geometry)
        .filter(|c| name(*c) == "GeometryPathType")
        .collect();
    if subpaths.len() > 1 {
        loss.note(
            part,
            "TextFrame",
            LossKind::Approximated,
            "compound_frame_path",
        );
    }
    let Some(first) = subpaths.first() else {
        return Ok((Vec::new(), false));
    };
    let pair = |s: &str| floats(s, 2).map(|v| [v[0], v[1]]);
    let mut points = Vec::new();
    if let Some(array) = child(*first, "PathPointArray") {
        for p in elements(array).filter(|c| name(*c) == "PathPointType") {
            let anchor = pair(attr(p, "Anchor").ok_or(IdmlError::InvalidGeometry)?)?;
            let left = attr(p, "LeftDirection")
                .map(pair)
                .transpose()?
                .unwrap_or(anchor);
            let right = attr(p, "RightDirection")
                .map(pair)
                .transpose()?
                .unwrap_or(anchor);
            if left != anchor || right != anchor {
                loss.note(
                    part,
                    "TextFrame",
                    LossKind::Approximated,
                    "curved_frame_path_bounds_only",
                );
            }
            points.push(PathPoint {
                anchor,
                left,
                right,
            });
        }
    }
    Ok((points, attr(*first, "PathOpen") == Some("true")))
}

pub(crate) const VJ: [(&str, VerticalJustification); 4] = [
    ("TopAlign", VerticalJustification::Top),
    ("CenterAlign", VerticalJustification::Center),
    ("BottomAlign", VerticalJustification::Bottom),
    ("JustifyAlign", VerticalJustification::Justify),
];
pub(crate) const FB: [(&str, FirstBaseline); 6] = [
    ("AscentOffset", FirstBaseline::Ascent),
    ("CapHeight", FirstBaseline::CapHeight),
    ("LeadingOffset", FirstBaseline::Leading),
    ("EmBoxHeight", FirstBaseline::EmBoxHeight),
    ("XHeight", FirstBaseline::XHeight),
    ("FixedHeight", FirstBaseline::Fixed),
];
pub(crate) const AS: [(&str, AutoSizing); 5] = [
    ("Off", AutoSizing::Off),
    ("HeightOnly", AutoSizing::HeightOnly),
    ("WidthOnly", AutoSizing::WidthOnly),
    ("HeightAndWidth", AutoSizing::HeightAndWidth),
    (
        "HeightAndWidthProportionally",
        AutoSizing::HeightAndWidthProportionally,
    ),
];

fn pick<T: Copy>(
    el: Node,
    key: &str,
    table: &[(&str, T)],
    default: T,
    part: &str,
    loss: &mut LossBook,
) -> T {
    let Some(v) = attr(el, key) else {
        return default;
    };
    table.iter().find(|(k, _)| *k == v).map_or_else(
        || {
            loss.note(
                part,
                "TextFramePreference",
                LossKind::Approximated,
                "frame_preference_value_unknown",
            );
            default
        },
        |(_, t)| *t,
    )
}

fn read_prefs(frame: Node, part: &str, loss: &mut LossBook) -> Result<FramePrefs> {
    let mut p = FramePrefs::default();
    let Some(el) = child(frame, "TextFramePreference") else {
        return Ok(p);
    };
    if let Some(v) = attr(el, "TextColumnCount") {
        p.column_count = v
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=1000).contains(n))
            .ok_or(IdmlError::InvalidGeometry)?;
    }
    if let Some(v) = attr(el, "TextColumnGutter") {
        p.column_gutter_pt = parse_f(v)?;
    }
    if let Some(v) = attr(el, "MinimumFirstBaselineOffset") {
        p.min_first_baseline_pt = parse_f(v)?;
    }
    p.vertical_justification = pick(
        el,
        "VerticalJustification",
        &VJ,
        p.vertical_justification,
        part,
        loss,
    );
    p.first_baseline = pick(el, "FirstBaselineOffset", &FB, p.first_baseline, part, loss);
    p.auto_size = pick(el, "AutoSizingType", &AS, p.auto_size, part, loss);
    if let Some(list) = child(el, "Properties").and_then(|c| child(c, "InsetSpacing")) {
        let items: Vec<f64> = elements(list)
            .filter(|c| name(*c) == "ListItem")
            .map(|c| parse_f(c.text().unwrap_or("")))
            .collect::<Result<_>>()?;
        p.inset_pt = match items.as_slice() {
            [t, l, b, r] => [*t, *l, *b, *r],
            [v] => [*v; 4],
            _ => return Err(IdmlError::InvalidGeometry),
        };
    } else if let Some(v) = attr(el, "InsetSpacing") {
        p.inset_pt = [parse_f(v)?; 4];
    }
    Ok(p)
}

fn language_class(value: &str) -> Option<&'static str> {
    let l = value.to_lowercase();
    let any = |names: &[&str]| names.iter().any(|k| l.contains(k));
    if any(&["arabic", "hebrew", "persian", "farsi", "urdu", "yiddish"]) {
        Some("rtl_language")
    } else if any(&["chinese", "japanese", "korean"]) {
        Some("cjk_language")
    } else if any(&[
        "thai",
        "hindi",
        "bengali",
        "tamil",
        "telugu",
        "gujarati",
        "punjabi",
        "kannada",
        "malayalam",
        "marathi",
        "nepali",
        "khmer",
        "lao",
        "burmese",
        "sanskrit",
    ]) {
        Some("complex_script_language")
    } else {
        None
    }
}

pub(crate) fn classify(key: &str, value: &str, out: &mut BTreeSet<&'static str>) {
    const CJK: [&str; 6] = [
        "Kinsoku",
        "Ruby",
        "Kenten",
        "Tatechuyoko",
        "Warichu",
        "Mojikumi",
    ];
    if key.ends_with("Direction") && value.contains("RightToLeft") {
        out.insert("rtl_direction");
    } else if key.ends_with("Orientation") && value.contains("Vertical") {
        out.insert("vertical_text");
    } else if key == "AppliedLanguage" {
        out.extend(language_class(value));
    } else if CJK.iter().any(|p| key.starts_with(p))
        && !matches!(value.trim(), "" | "n" | "None" | "false" | "0" | "$ID/None")
    {
        out.insert("cjk_typography");
    }
}

/// Own attributes plus the descendants of `Properties` children (where local overrides live).
fn scan_composition(n: Node, out: &mut BTreeSet<&'static str>) {
    for a in n.attributes() {
        classify(a.name(), a.value(), out);
    }
    for p in elements(n).filter(|c| name(*c) == "Properties") {
        for d in p.descendants().filter(|d| d.is_element()) {
            classify(name(d), d.text().unwrap_or(""), out);
        }
    }
}

struct StoryBuilder<'a> {
    part: &'a str,
    text: &'a str,
    loss: &'a mut LossBook,
    comp: BTreeSet<&'static str>,
    paras: Vec<ParaModel>,
    open: bool,
    /// Logical model bytes charged so far across all stories (see `Limits::max_model_bytes`).
    charged: &'a mut u64,
    max_charged: u64,
}

impl StoryBuilder<'_> {
    /// Every paragraph and run is charged what its export writes, shared data included, so the
    /// size of anything this model can later be exported to is bounded by `max_model_bytes`.
    fn charge(&mut self, bytes: u64) -> Result<()> {
        *self.charged = self.charged.saturating_add(bytes);
        if *self.charged > self.max_charged {
            return Err(IdmlError::BudgetExceeded("model_bytes"));
        }
        Ok(())
    }

    /// Opens a paragraph from the range template. The template's style, attributes and
    /// `Properties` are shared (`Arc`), so a new paragraph costs pointers, not a copy.
    fn ensure_paragraph(&mut self, template: &ParaModel, weight: u64) -> Result<()> {
        if !self.open || self.paras.is_empty() {
            self.charge(weight.saturating_add(PARA_OVERHEAD))?;
            self.paras.push(template.clone());
            self.open = true;
        }
        Ok(())
    }

    fn paragraph_range(&mut self, n: Node) -> Result<()> {
        scan_composition(n, &mut self.comp);
        let template = ParaModel {
            style: Arc::from(attr(n, "AppliedParagraphStyle").unwrap_or_default()),
            attrs: Arc::new(extras(n, &["AppliedParagraphStyle"])),
            props: child(n, "Properties").map(|p| Arc::new(opaque(p, self.text))),
            runs: Vec::new(),
        };
        let weight = range_weight(&template.style, &template.attrs, template.props.as_deref());
        for c in elements(n) {
            match name(c) {
                "CharacterStyleRange" => self.character_range(c, &template, weight)?,
                "Properties" => {}
                other => self.loss.note(
                    self.part,
                    other,
                    LossKind::Unsupported,
                    "paragraph_range_element_not_interpreted",
                ),
            }
        }
        Ok(())
    }

    fn character_range(&mut self, n: Node, template: &ParaModel, para_weight: u64) -> Result<()> {
        scan_composition(n, &mut self.comp);
        let props = child(n, "Properties");
        let language = attr(n, "AppliedLanguage")
            .map(Arc::<str>::from)
            .or_else(|| {
                props
                    .and_then(|p| child(p, "AppliedLanguage"))
                    .and_then(|l| l.text())
                    .map(|s| Arc::from(s.trim()))
            });
        // Built once per range; each Content / Br below clones pointers only.
        let proto = RunModel {
            char_style: Arc::from(attr(n, "AppliedCharacterStyle").unwrap_or_default()),
            attrs: Arc::new(extras(n, &["AppliedCharacterStyle"])),
            props: props.map(|p| Arc::new(opaque(p, self.text))),
            text: String::new(),
            br: false,
            special: false,
            language,
            span: None,
            inline: None,
        };
        let weight = range_weight(&proto.char_style, &proto.attrs, proto.props.as_deref());
        let mut unterminated: Option<usize> = None;
        for c in elements(n) {
            match name(c) {
                "Content" => {
                    let mut run = proto.clone();
                    for t in c.children() {
                        if t.is_text() {
                            run.text.push_str(t.text().unwrap_or_default());
                        } else if t.is_pi() {
                            run.special = true;
                        }
                    }
                    if run.special {
                        self.loss.note(
                            self.part,
                            "Content",
                            LossKind::Unsupported,
                            "special_character_dropped",
                        );
                    }
                    run.span = Some(c.range());
                    self.ensure_paragraph(template, para_weight)?;
                    self.charge(
                        weight
                            .saturating_add(RUN_OVERHEAD)
                            .saturating_add(run.text.len() as u64),
                    )?;
                    if let Some(p) = self.paras.last_mut() {
                        p.runs.push(run);
                        unterminated = p.runs.len().checked_sub(1);
                    }
                }
                "Br" => {
                    match unterminated.take() {
                        Some(i) => {
                            if let Some(r) = self.paras.last_mut().and_then(|p| p.runs.get_mut(i)) {
                                r.br = true;
                            }
                        }
                        None => {
                            self.ensure_paragraph(template, para_weight)?;
                            self.charge(weight.saturating_add(RUN_OVERHEAD))?;
                            if let Some(p) = self.paras.last_mut() {
                                p.runs.push(RunModel {
                                    br: true,
                                    ..proto.clone()
                                });
                            }
                        }
                    }
                    self.open = false;
                }
                "Properties" => {}
                el @ ("Table" | "Footnote" | "Endnote" | "EndnoteRange" | "TextVariableInstance") => {
                    // Kept in place (no text of its own); `import_longdoc` interprets it.
                    let xml = opaque(c, self.text);
                    self.ensure_paragraph(template, para_weight)?;
                    self.charge(
                        weight
                            .saturating_add(RUN_OVERHEAD)
                            .saturating_add(xml.xml.len() as u64),
                    )?;
                    self.loss.note(self.part, el, LossKind::PreservedOpaque, "inline_object_kept");
                    if let Some(p) = self.paras.last_mut() {
                        p.runs.push(RunModel {
                            inline: Some(Arc::new(xml)),
                            ..proto.clone()
                        });
                    }
                    // A following Br ends the paragraph after the object, not after earlier text.
                    unterminated = None;
                }
                other => self.loss.note(
                    self.part,
                    other,
                    LossKind::Unsupported,
                    "character_range_element_not_interpreted",
                ),
            }
        }
        Ok(())
    }
}

fn read_story(
    part: &IdmlPart,
    limits: &Limits,
    loss: &mut LossBook,
    charged: &mut u64,
    cancel: &CancellationToken,
) -> Result<StoryModel> {
    let text = text_of(part)?;
    let doc = parse_xml(text, limits)?;
    let story = inner(doc.root_element(), "Story")?;
    let id = attr(story, "Self")
        .ok_or(IdmlError::NotIdml("story_self"))?
        .to_owned();
    let mut b = StoryBuilder {
        part: &part.name,
        text,
        loss,
        comp: BTreeSet::new(),
        paras: Vec::new(),
        open: false,
        charged,
        max_charged: limits.max_model_bytes,
    };
    scan_composition(story, &mut b.comp);
    let (mut head, mut right_to_left) = (Vec::new(), false);
    for c in elements(story) {
        canceled(cancel)?;
        if name(c) == "ParagraphStyleRange" {
            b.paragraph_range(c)?;
            continue;
        }
        if name(c) == "StoryPreference" {
            right_to_left = attr(c, "StoryDirection") == Some("RightToLeftDirection");
            scan_composition(c, &mut b.comp);
        } else {
            b.loss.note(
                &part.name,
                name(c),
                LossKind::PreservedOpaque,
                "story_element_not_interpreted",
            );
        }
        head.push(opaque(c, text));
    }
    let StoryBuilder {
        comp, paras, loss, ..
    } = b;
    for code in &comp {
        loss.note(&part.name, "Story", LossKind::UnsupportedComposition, code);
    }
    Ok(StoryModel {
        id,
        part: part.name.clone(),
        part_sha256: part.sha256,
        attrs: extras(story, &["Self"]),
        head,
        right_to_left,
        paragraphs: paras,
        composition: comp.into_iter().collect(),
    })
}

/// Validates the frame links of the model and returns the chains (head first, document order) as
/// `(spread index, frame index)`. A link to a frame outside the supported subset ends the chain
/// there and is reported; a dangling story, a non-reciprocal link or a cycle is a typed error.
pub(crate) fn thread_chains(
    model: &IdmlModel,
    loss: &mut LossBook,
    cancel: &CancellationToken,
) -> Result<Vec<Vec<Pos>>> {
    let mut at: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (si, s) in model.spreads.iter().enumerate() {
        for (fi, f) in s.frames.iter().enumerate() {
            if at.insert(f.id.as_str(), (si, fi)).is_some() {
                return Err(IdmlError::NotIdml("duplicate_frame_self"));
            }
            if !model.stories.iter().any(|st| st.id == f.story_id) {
                return Err(IdmlError::DanglingStory);
            }
        }
    }
    let frame = |(si, fi): (usize, usize)| &model.spreads[si].frames[fi];
    let mut linked: BTreeMap<Pos, Links> = BTreeMap::new();
    for &pos in at.values() {
        canceled(cancel)?;
        let f = frame(pos);
        let mut resolve = |l: &Option<String>| -> Option<(usize, usize)> {
            let target = l.as_deref()?;
            let found = at.get(target).copied();
            if found.is_none() {
                loss.note(
                    &model.spreads[pos.0].part,
                    "TextFrame",
                    LossKind::Approximated,
                    "thread_link_outside_subset",
                );
            }
            found
        };
        linked.insert(pos, (resolve(&f.previous), resolve(&f.next)));
    }
    for (&pos, &(prev, next)) in &linked {
        if next.is_some_and(|n| linked[&n].0 != Some(pos))
            || prev.is_some_and(|p| linked[&p].1 != Some(pos))
        {
            return Err(IdmlError::ThreadNotReciprocal);
        }
    }
    let mut chains = Vec::new();
    let mut visited = 0usize;
    for (si, s) in model.spreads.iter().enumerate() {
        for fi in 0..s.frames.len() {
            if linked[&(si, fi)].0.is_some() {
                continue;
            }
            let mut chain = vec![(si, fi)];
            while let Some(next) = linked[chain.last().unwrap_or(&(si, fi))].1 {
                chain.push(next);
                if chain.len() > linked.len() {
                    return Err(IdmlError::CyclicThread);
                }
            }
            visited = visited.saturating_add(chain.len());
            chains.push(chain);
        }
    }
    if visited != linked.len() {
        return Err(IdmlError::CyclicThread);
    }
    Ok(chains)
}
