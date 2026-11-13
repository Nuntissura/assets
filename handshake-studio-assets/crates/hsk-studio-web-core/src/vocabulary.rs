//! Tag libraries and the attribute vocabulary (lane SMB-WEB-CODE-1; [STU-WEB-017] to [STU-WEB-027]): the tag-library,
//! tag-definition, formatting-rule and attribute-definition records, the closed value-type enumeration, cross-tag
//! attribute groups, the case rules, the generic-inspector fallback and third-party registration.
//!
//! Shipped data. The ten library records of [STU-WEB-018] are the spec's table (`display_name` is the `library_id`:
//! the table names no display name; `tag_chooser_id` is null). Loaded tag definitions exist for the `html` library
//! only: the HTML elements of the HTML Standard (WHATWG, https://html.spec.whatwg.org/, "Copyright WHATWG (Apple,
//! Google, Mozilla, Microsoft)", CC BY 4.0; portions in source code BSD 3-Clause: the crate's NOTICE) with their
//! void/non-void shape, a Studio-declared formatting class per element and a subset of the attributes and enumerated
//! values that standard defines, plus two cross-tag groups (global attributes, event handlers). The `referrerpolicy`
//! tokens are the W3C Referrer Policy's (NOTICE). Changes: a subset, Studio formatting classes, value types mapped by
//! Studio. The vendor's 470 definitions, 9,741 attribute definitions and 14,733 enumerated values are not in the
//! spec and are not reproduced: every library's declared-versus-loaded delta is a load diagnostic
//! ([`TagRegistry::load_diagnostics`]), never silently dropped. Formatting classes are Studio declarations, chosen so a
//! format is rendering-neutral (inline-level and replaced elements are `Inline`); the vendor's per-tag values are
//! UNVERIFIED (D5). No shipped definition declares a property inspector, so every tag resolves to the generic
//! attribute-table inspector ([STU-WEB-026]).
//!
//! Spec reading: [STU-WEB-018] says the `html` and `templates` libraries apply to "all seven template types" while
//! [STU-WEB-013] has eight TEMPLATE rows (23 and 33 to 39). Both libraries apply to all eight here (a template whose
//! own markup resolved no HTML tag would be unusable); the count mismatch is a spec question, not a choice of rows.
#![deny(clippy::arithmetic_side_effects)]

use crate::doc_types::{CodeError, DOCUMENT_TYPES, document_type};

/// [STU-WEB-022], closed: exactly twenty value types, normalised to lower case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    Text,
    Unspecified,
    Enumerated,
    Color,
    CssStyle,
    Style,
    CssId,
    Flag,
    RelativePath,
    Font,
    CssClass,
    Xpath,
    FilePath,
    Directory,
    FileName,
    WidthSrc,
    HeightSrc,
    XpathContext,
    WidthData,
    HeightData,
}

impl ValueType {
    pub const ALL: [Self; 20] = [
        Self::Text,
        Self::Unspecified,
        Self::Enumerated,
        Self::Color,
        Self::CssStyle,
        Self::Style,
        Self::CssId,
        Self::Flag,
        Self::RelativePath,
        Self::Font,
        Self::CssClass,
        Self::Xpath,
        Self::FilePath,
        Self::Directory,
        Self::FileName,
        Self::WidthSrc,
        Self::HeightSrc,
        Self::XpathContext,
        Self::WidthData,
        Self::HeightData,
    ];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Unspecified => "unspecified",
            Self::Enumerated => "enumerated",
            Self::Color => "color",
            Self::CssStyle => "cssstyle",
            Self::Style => "style",
            Self::CssId => "cssid",
            Self::Flag => "flag",
            Self::RelativePath => "relativepath",
            Self::Font => "font",
            Self::CssClass => "cssclass",
            Self::Xpath => "xpath",
            Self::FilePath => "filepath",
            Self::Directory => "directory",
            Self::FileName => "filename",
            Self::WidthSrc => "width:src",
            Self::HeightSrc => "height:src",
            Self::XpathContext => "xpath_context",
            Self::WidthData => "width:data",
            Self::HeightData => "height:data",
        }
    }

    /// The source vocabulary normalised to lower case ([STU-WEB-022]); anything else is no member.
    pub fn parse(token: &str) -> Option<Self> {
        if token.len() > 16 {
            return None;
        }
        let lower = token.to_ascii_lowercase();
        Self::ALL.into_iter().find(|v| v.token() == lower)
    }

    /// The four types whose value is read from the referenced asset ([STU-WEB-022]).
    pub const fn derived(self) -> bool {
        matches!(self, Self::WidthSrc | Self::HeightSrc | Self::WidthData | Self::HeightData)
    }
}

/// [STU-WEB-019] `has_end_tag`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HasEndTag {
    Yes,
    No,
    /// The self-closing form is required in XML-family documents.
    Xml,
}

impl HasEndTag {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Xml => "xml",
        }
    }
}

/// [STU-WEB-019] `tag_type` (`None` is the null member).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagType {
    Empty,
    Nonempty,
}

impl TagType {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Nonempty => "nonempty",
        }
    }
}

/// [STU-WEB-020]: the closed set of eleven formatting keys.
pub const FORMATTING_KEYS: [&str; 11] = [
    "nlbeforetag",
    "nlbeforetag_min",
    "nlbeforetag_max",
    "nlaftertag",
    "nlaftertag_min",
    "nlaftertag_max",
    "nlbeforecontents",
    "nlaftercontents",
    "indentcontents",
    "formatcontents",
    "preserveattrcase",
];

/// [STU-WEB-020]: one tag's formatting rules. `formatcontents: false` keeps the contents verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormattingRules {
    pub nlbeforetag: u8,
    pub nlbeforetag_min: u8,
    pub nlbeforetag_max: u8,
    pub nlaftertag: u8,
    pub nlaftertag_min: u8,
    pub nlaftertag_max: u8,
    pub nlbeforecontents: u8,
    pub nlaftercontents: u8,
    pub indentcontents: bool,
    pub formatcontents: bool,
    pub preserveattrcase: bool,
}

impl FormattingRules {
    /// No newline anywhere, contents formatted, not indented: an inline element.
    pub const INLINE: Self = Self { nlbeforetag: 0, nlbeforetag_min: 0, nlbeforetag_max: 2, nlaftertag: 0, nlaftertag_min: 0, nlaftertag_max: 2, nlbeforecontents: 0, nlaftercontents: 0, indentcontents: false, formatcontents: true, preserveattrcase: false };

    /// The record from key/value pairs: an unknown key or a value outside its domain is refused, never ignored. Keys
    /// not given take [`Self::INLINE`]'s value.
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Result<Self, CodeError> {
        let mut r = Self::INLINE;
        for (key, value) in pairs {
            let bit = |v: &str| match v {
                "0" => Ok(0u8),
                "1" => Ok(1u8),
                _ => Err(CodeError::new("formatting_value_invalid")),
            };
            let yes_no = |v: &str| match v {
                "yes" => Ok(true),
                "no" => Ok(false),
                _ => Err(CodeError::new("formatting_value_invalid")),
            };
            let fixed = |v: &str, want: &str, n: u8| if v == want { Ok(n) } else { Err(CodeError::new("formatting_value_invalid")) };
            match *key {
                "nlbeforetag" => r.nlbeforetag = bit(value)?,
                "nlbeforetag_min" => r.nlbeforetag_min = fixed(value, "0", 0)?,
                "nlbeforetag_max" => r.nlbeforetag_max = fixed(value, "2", 2)?,
                "nlaftertag" => r.nlaftertag = bit(value)?,
                "nlaftertag_min" => r.nlaftertag_min = fixed(value, "0", 0)?,
                "nlaftertag_max" => r.nlaftertag_max = fixed(value, "2", 2)?,
                "nlbeforecontents" => r.nlbeforecontents = bit(value)?,
                "nlaftercontents" => r.nlaftercontents = bit(value)?,
                "indentcontents" => r.indentcontents = yes_no(value)?,
                "formatcontents" => r.formatcontents = yes_no(value)?,
                "preserveattrcase" => r.preserveattrcase = fixed(value, "yes", 1)? == 1,
                _ => return Err(CodeError::new("formatting_key_unknown")),
            }
        }
        Ok(r)
    }

    /// The record as key/value pairs in [`FORMATTING_KEYS`] order (`preserveattrcase` only when present).
    pub fn pairs(&self) -> Vec<(&'static str, String)> {
        let yn = |b: bool| if b { "yes" } else { "no" }.to_string();
        let mut out = vec![
            ("nlbeforetag", self.nlbeforetag.to_string()),
            ("nlbeforetag_min", self.nlbeforetag_min.to_string()),
            ("nlbeforetag_max", self.nlbeforetag_max.to_string()),
            ("nlaftertag", self.nlaftertag.to_string()),
            ("nlaftertag_min", self.nlaftertag_min.to_string()),
            ("nlaftertag_max", self.nlaftertag_max.to_string()),
            ("nlbeforecontents", self.nlbeforecontents.to_string()),
            ("nlaftercontents", self.nlaftercontents.to_string()),
            ("indentcontents", yn(self.indentcontents)),
            ("formatcontents", yn(self.formatcontents)),
        ];
        if self.preserveattrcase {
            out.push(("preserveattrcase", "yes".to_string()));
        }
        out
    }
}

/// [STU-WEB-021] `allowed_values` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowedValue {
    pub value: String,
    pub caption: Option<String>,
}

/// [STU-WEB-021]: one attribute definition. `allowed_values` is non-empty only for [`ValueType::Enumerated`], in
/// declared order (never sorted, [STU-WEB-023]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeDefinition {
    pub name: String,
    pub value_type: ValueType,
    pub case_sensitive: bool,
    pub allowed_values: Vec<AllowedValue>,
}

impl AttributeDefinition {
    pub fn new(name: &str, value_type: ValueType, allowed: &[&str]) -> Result<Self, CodeError> {
        checked_name(name)?;
        if allowed.is_empty() == (value_type == ValueType::Enumerated) {
            return Err(CodeError::new(if allowed.is_empty() { "allowed_values_missing" } else { "allowed_values_without_enumerated" }));
        }
        if allowed.len() > MAX_VALUES || allowed.iter().any(|v| v.len() > MAX_NAME) {
            return Err(CodeError::new("vocabulary_too_large"));
        }
        Ok(Self { name: name.to_string(), value_type, case_sensitive: false, allowed_values: allowed.iter().map(|v| AllowedValue { value: (*v).to_string(), caption: None }).collect() })
    }

    /// Whether `value` is one of the allowed values ([STU-WEB-023]); `true` for a non-enumerated attribute. HTML
    /// enumerated values compare ASCII case-insensitively unless the definition is case-sensitive.
    pub fn allows(&self, value: &str) -> bool {
        self.value_type != ValueType::Enumerated
            || self.allowed_values.iter().any(|a| if self.case_sensitive { a.value == value } else { a.value.eq_ignore_ascii_case(value) })
    }
}

/// [STU-WEB-024]: a named attribute set injected into every tag that declares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeGroup {
    pub group_id: String,
    pub attributes: Vec<AttributeDefinition>,
}

/// [STU-WEB-019]: one tag definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagDefinition {
    pub tag_name: String,
    pub library_id: String,
    pub has_end_tag: HasEndTag,
    pub tag_type: Option<TagType>,
    pub formatting_rules: FormattingRules,
    pub property_inspector_id: Option<String>,
    pub attributes: Vec<AttributeDefinition>,
    /// Cross-tag groups this tag declares ([STU-WEB-024]).
    pub groups: Vec<String>,
}

/// [STU-WEB-017]: one tag library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagLibrary {
    pub library_id: String,
    pub display_name: String,
    pub applies_to_document_types: Vec<String>,
    pub namespace_prefix: Option<String>,
    pub declared_tag_count: u32,
    pub tag_chooser_id: Option<String>,
}

/// [STU-WEB-026]: the inspector surface a tag resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectorBinding {
    Declared(String),
    /// The generic attribute-table inspector: a tag with no declared inspector never shows nothing.
    GenericAttributeTable,
}

impl InspectorBinding {
    pub fn token(&self) -> &str {
        match self {
            Self::Declared(id) => id,
            Self::GenericAttributeTable => "generic_attribute_table",
        }
    }
}

/// A finding made while loading or registering vocabulary ([STU-WEB-018], [STU-WEB-024]): reported, never silent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadDiagnostic {
    pub code: &'static str,
    pub library_id: String,
    /// The tag or group the finding is about, when there is one.
    pub subject: Option<String>,
    pub declared: Option<u32>,
    pub loaded: Option<u32>,
}

/// Registration bounds (hostile plugin vocabularies).
pub const MAX_LIBRARIES: usize = 64;
pub const MAX_TAGS: usize = 8192;
pub const MAX_ATTRIBUTES_PER_TAG: usize = 512;
pub const MAX_VALUES: usize = 1024;
pub const MAX_GROUPS: usize = 64;
const MAX_NAME: usize = 128;
const MAX_TYPES_PER_LIBRARY: usize = 64;

/// A tag or attribute name: `[A-Za-z_][A-Za-z0-9:._-]*`, at most 128 bytes (namespaced forms such as `asp:Button`).
fn checked_name(name: &str) -> Result<(), CodeError> {
    let b = name.as_bytes();
    let first = b.first().is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_');
    if !first || b.len() > MAX_NAME || !b.iter().all(|c| c.is_ascii_alphanumeric() || matches!(c, b':' | b'.' | b'_' | b'-')) {
        return Err(CodeError::new("vocabulary_name_invalid"));
    }
    Ok(())
}

fn checked_id(id: &str) -> Result<(), CodeError> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_') {
        return Err(CodeError::new("vocabulary_id_invalid"));
    }
    Ok(())
}

/// The loaded vocabulary: the shipped libraries plus registered ones ([STU-WEB-027]).
#[derive(Clone, Debug)]
pub struct TagRegistry {
    libraries: Vec<TagLibrary>,
    tags: Vec<TagDefinition>,
    groups: Vec<AttributeGroup>,
    /// Libraries registered after the shipped set (they never edit a shipped library).
    shipped_count: usize,
    diagnostics: Vec<LoadDiagnostic>,
}

impl TagRegistry {
    /// The shipped vocabulary (module docs).
    pub fn shipped() -> Self {
        let libraries = shipped_libraries();
        let groups = vec![
            AttributeGroup { group_id: "html_global".into(), attributes: seeds(GLOBAL_ATTRIBUTES) },
            AttributeGroup { group_id: "html_events".into(), attributes: EVENT_HANDLERS.iter().map(|n| attr(n, ValueType::Text, &[])).collect() },
        ];
        let tags = HTML_TAGS.iter().map(TagSeed::definition).collect();
        let shipped_count = libraries.len();
        let mut registry = Self { libraries, tags, groups, shipped_count, diagnostics: Vec::new() };
        registry.diagnostics = registry.compute_diagnostics(0);
        registry
    }

    pub fn libraries(&self) -> &[TagLibrary] {
        &self.libraries
    }

    pub fn tags(&self) -> &[TagDefinition] {
        &self.tags
    }

    pub fn groups(&self) -> &[AttributeGroup] {
        &self.groups
    }

    /// Declared-versus-loaded deltas per library and cross-tag collisions ([STU-WEB-018], [STU-WEB-024]).
    pub fn load_diagnostics(&self) -> &[LoadDiagnostic] {
        &self.diagnostics
    }

    /// The libraries a document type resolves against: exactly those whose `applies_to_document_types` holds it.
    pub fn libraries_for(&self, type_id: &str) -> Vec<&TagLibrary> {
        self.libraries.iter().filter(|l| l.applies_to_document_types.iter().any(|t| t == type_id)).collect()
    }

    /// The definition of tag `name` in a document of `type_id` ([STU-WEB-017], [STU-WEB-025]): case-insensitive in
    /// HTML-family types, exact in the XML family; libraries in registry order, the first definition wins.
    pub fn tag(&self, type_id: &str, name: &str) -> Option<&TagDefinition> {
        let xml = document_type(type_id).is_some_and(|t| t.xml_family());
        // No allocation per lookup (the formatter and validator ask once per element).
        let active = |lib: &str| self.libraries.iter().any(|l| l.library_id == lib && l.applies_to_document_types.iter().any(|t| t == type_id));
        self.tags.iter().find(|t| (if xml { t.tag_name == name } else { t.tag_name.eq_ignore_ascii_case(name) }) && active(&t.library_id))
    }

    /// Every tag definition a document type resolves, in registry order.
    pub fn tags_for(&self, type_id: &str) -> Vec<&TagDefinition> {
        let libs = self.libraries_for(type_id);
        self.tags.iter().filter(|t| libs.iter().any(|l| l.library_id == t.library_id)).collect()
    }

    /// A tag's attributes with its declared cross-tag groups merged in ([STU-WEB-024]): the tag's own definition wins a
    /// name collision (the collision is a load diagnostic). Own attributes first, then each group's in group order.
    pub fn attributes_of(&self, tag: &TagDefinition) -> Vec<AttributeDefinition> {
        let mut out = tag.attributes.clone();
        for group in tag.groups.iter().filter_map(|g| self.groups.iter().find(|x| &x.group_id == g)) {
            for a in &group.attributes {
                if !out.iter().any(|o| same_attribute(tag, o, &a.name)) {
                    out.push(a.clone());
                }
            }
        }
        out
    }

    /// The attribute definition of `name` on `tag` ([STU-WEB-025]: the definition's `case_sensitive`, overridden by the
    /// tag's `preserveattrcase`).
    pub fn attribute(&self, tag: &TagDefinition, name: &str) -> Option<AttributeDefinition> {
        self.attribute_ref(tag, name).cloned()
    }

    /// [`Self::attribute`] without a copy: the tag's own definitions first, then its groups' (review F2: no merged list
    /// per attribute).
    pub fn attribute_ref<'a>(&'a self, tag: &'a TagDefinition, name: &str) -> Option<&'a AttributeDefinition> {
        tag.attributes.iter().find(|a| same_attribute(tag, a, name)).or_else(|| tag.groups.iter().filter_map(|g| self.groups.iter().find(|x| &x.group_id == g)).flat_map(|g| g.attributes.iter()).find(|a| same_attribute(tag, a, name)))
    }

    /// [STU-WEB-026].
    pub fn inspector(&self, tag: &TagDefinition) -> InspectorBinding {
        tag.property_inspector_id.clone().map_or(InspectorBinding::GenericAttributeTable, InspectorBinding::Declared)
    }

    /// [STU-WEB-027]: registers a library with its tags and groups without editing a shipped one. Refused whole (nothing
    /// registered) on an id already present, a type the set does not have, a tag of another library, a group a tag
    /// names that neither exists nor comes with it, a malformed name or a bound. Returns the new diagnostics. The
    /// consent gate of a plugin capability is the caller's (no plugin host here).
    pub fn register(&mut self, library: TagLibrary, tags: Vec<TagDefinition>, groups: Vec<AttributeGroup>) -> Result<Vec<LoadDiagnostic>, CodeError> {
        checked_id(&library.library_id)?;
        if self.libraries.len() >= MAX_LIBRARIES || self.tags.len().saturating_add(tags.len()) > MAX_TAGS || self.groups.len().saturating_add(groups.len()) > MAX_GROUPS {
            return Err(CodeError::new("vocabulary_too_large"));
        }
        if self.libraries.iter().any(|l| l.library_id == library.library_id) {
            return Err(CodeError::new("library_id_taken"));
        }
        if library.applies_to_document_types.len() > MAX_TYPES_PER_LIBRARY || library.applies_to_document_types.iter().any(|t| document_type(t).is_none()) {
            return Err(CodeError::new("document_type_unknown"));
        }
        if library.display_name.chars().count() > MAX_NAME || library.namespace_prefix.as_ref().is_some_and(|p| p.len() > MAX_NAME) {
            return Err(CodeError::new("vocabulary_name_invalid"));
        }
        for (i, g) in groups.iter().enumerate() {
            checked_id(&g.group_id)?;
            if self.groups.iter().chain(groups.iter().take(i)).any(|x| x.group_id == g.group_id) {
                return Err(CodeError::new("group_id_taken"));
            }
            if g.attributes.len() > MAX_ATTRIBUTES_PER_TAG {
                return Err(CodeError::new("vocabulary_too_large"));
            }
            for a in &g.attributes {
                checked_attribute(a)?;
            }
        }
        for t in &tags {
            checked_name(&t.tag_name)?;
            if t.library_id != library.library_id {
                return Err(CodeError::new("tag_library_mismatch"));
            }
            if t.attributes.len() > MAX_ATTRIBUTES_PER_TAG || t.groups.len() > MAX_GROUPS {
                return Err(CodeError::new("vocabulary_too_large"));
            }
            for a in &t.attributes {
                checked_attribute(a)?;
            }
            if t.groups.iter().any(|g| !self.groups.iter().chain(&groups).any(|x| &x.group_id == g)) {
                return Err(CodeError::new("group_unknown"));
            }
        }
        let first_tag = self.tags.len();
        self.libraries.push(library);
        self.tags.extend(tags);
        self.groups.extend(groups);
        let fresh = self.compute_diagnostics(first_tag);
        let at = self.libraries.len().saturating_sub(1);
        let new: Vec<LoadDiagnostic> = fresh.into_iter().filter(|d| self.libraries.get(at).is_some_and(|l| l.library_id == d.library_id)).collect();
        self.diagnostics.extend(new.iter().cloned());
        Ok(new)
    }

    /// Whether `library_id` was registered rather than shipped.
    pub fn is_registered(&self, library_id: &str) -> bool {
        self.libraries.iter().position(|l| l.library_id == library_id).is_some_and(|i| i >= self.shipped_count)
    }

    fn compute_diagnostics(&self, first_tag: usize) -> Vec<LoadDiagnostic> {
        let mut out = Vec::new();
        for lib in &self.libraries {
            let loaded = u32::try_from(self.tags.iter().filter(|t| t.library_id == lib.library_id).count()).unwrap_or(u32::MAX);
            if loaded != lib.declared_tag_count {
                out.push(LoadDiagnostic { code: "library_definition_delta", library_id: lib.library_id.clone(), subject: None, declared: Some(lib.declared_tag_count), loaded: Some(loaded) });
            }
        }
        for tag in self.tags.iter().skip(first_tag) {
            for group in tag.groups.iter().filter_map(|g| self.groups.iter().find(|x| &x.group_id == g)) {
                for a in group.attributes.iter().filter(|a| tag.attributes.iter().any(|o| same_attribute(tag, o, &a.name))) {
                    out.push(LoadDiagnostic { code: "cross_tag_attribute_collision", library_id: tag.library_id.clone(), subject: Some(format!("{}/{}/{}", tag.tag_name, group.group_id, a.name)), declared: None, loaded: None });
                }
            }
        }
        out
    }
}

fn checked_attribute(a: &AttributeDefinition) -> Result<(), CodeError> {
    checked_name(&a.name)?;
    if a.allowed_values.is_empty() == (a.value_type == ValueType::Enumerated) {
        return Err(CodeError::new(if a.allowed_values.is_empty() { "allowed_values_missing" } else { "allowed_values_without_enumerated" }));
    }
    if a.allowed_values.len() > MAX_VALUES || a.allowed_values.iter().any(|v| v.value.len() > MAX_NAME || v.caption.as_ref().is_some_and(|c| c.len() > MAX_NAME)) {
        return Err(CodeError::new("vocabulary_too_large"));
    }
    Ok(())
}

fn same_attribute(tag: &TagDefinition, def: &AttributeDefinition, name: &str) -> bool {
    if tag.formatting_rules.preserveattrcase || def.case_sensitive { def.name == name } else { def.name.eq_ignore_ascii_case(name) }
}

/// The ten libraries of [STU-WEB-018].
fn shipped_libraries() -> Vec<TagLibrary> {
    let templates: Vec<&str> = DOCUMENT_TYPES.iter().filter(|t| t.is_template()).map(|t| t.type_id).collect();
    let with_templates = |base: &[&str], tail: &[&str]| base.iter().chain(&templates).chain(tail).map(|s| (*s).to_string()).collect::<Vec<_>>();
    let list = |ids: &[&str]| ids.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
    let lib = |id: &str, prefix: Option<&str>, declared: u32, applies: Vec<String>| TagLibrary {
        library_id: id.to_string(),
        display_name: id.to_string(),
        applies_to_document_types: applies,
        namespace_prefix: prefix.map(str::to_string),
        declared_tag_count: declared,
        tag_chooser_id: None,
    };
    vec![
        lib("html", None, 121, with_templates(&["html", "asp_js", "asp_vb", "aspnet_csharp", "aspnet_vb", "coldfusion", "jsp", "php", "library_item", "xslt_page", "xslt_fragment"], &[])),
        lib("cfml", None, 111, list(&["coldfusion", "coldfusion_component", "template_coldfusion"])),
        lib("aspnet", Some("<asp:"), 69, list(&["aspnet_csharp", "aspnet_vb", "template_aspnet_csharp", "template_aspnet_vb"])),
        lib("jsp", Some("<jsp:"), 15, list(&["jsp", "template_jsp"])),
        lib("jrun", Some("<jrun:"), 22, list(&["jsp", "template_jsp"])),
        lib("asp", None, 2, list(&["asp_js", "asp_vb", "template_asp_js", "template_asp_vb"])),
        lib("php", None, 8, list(&["php", "template_php"])),
        lib("templates", None, 2, with_templates(&["html"], &["xslt_page", "xslt_fragment"])),
        lib("xslt", Some("<xsl:"), 35, list(&["xslt_page", "xslt_fragment"])),
        lib("svg", Some("<http://www.w3.org/2000/svg:"), 81, list(&["svg", "html", "template_html", "php"])),
    ]
}

/// Studio-declared formatting classes of the shipped HTML definitions (module docs).
#[derive(Clone, Copy)]
enum Fmt {
    /// `html`: its children on their own lines, not indented.
    Root,
    /// Children on their own lines, indented one level.
    Container,
    /// Own line, inline contents.
    Block,
    /// Own line, contents verbatim (`pre`, `script`, `style`, raw-text elements).
    Verbatim,
    /// In the text flow, contents verbatim (`textarea`).
    VerbatimInline,
    Inline,
}

impl Fmt {
    const fn rules(self) -> FormattingRules {
        let block = FormattingRules { nlbeforetag: 1, nlaftertag: 1, ..FormattingRules::INLINE };
        match self {
            Self::Root => FormattingRules { nlbeforecontents: 1, nlaftercontents: 1, ..block },
            Self::Container => FormattingRules { nlbeforecontents: 1, nlaftercontents: 1, indentcontents: true, ..block },
            Self::Block => block,
            Self::Verbatim => FormattingRules { formatcontents: false, ..block },
            Self::VerbatimInline => FormattingRules { formatcontents: false, ..FormattingRules::INLINE },
            Self::Inline => FormattingRules::INLINE,
        }
    }
}

type AttrSeed = (&'static str, ValueType, &'static [&'static str]);

struct TagSeed(&'static str, Fmt, bool, &'static [AttrSeed]);

impl TagSeed {
    fn definition(&self) -> TagDefinition {
        let TagSeed(name, fmt, void, attrs) = *self;
        TagDefinition {
            tag_name: name.to_string(),
            library_id: "html".into(),
            has_end_tag: if void { HasEndTag::No } else { HasEndTag::Yes },
            tag_type: Some(if void { TagType::Empty } else { TagType::Nonempty }),
            formatting_rules: fmt.rules(),
            property_inspector_id: None,
            attributes: seeds(attrs),
            groups: vec!["html_global".into(), "html_events".into()],
        }
    }
}

fn attr(name: &str, value_type: ValueType, allowed: &[&str]) -> AttributeDefinition {
    AttributeDefinition { name: name.to_string(), value_type, case_sensitive: false, allowed_values: allowed.iter().map(|v| AllowedValue { value: (*v).to_string(), caption: None }).collect() }
}

fn seeds(list: &[AttrSeed]) -> Vec<AttributeDefinition> {
    list.iter().map(|(n, t, a)| attr(n, *t, a)).collect()
}

use ValueType::{CssClass, CssId, CssStyle, Enumerated as E, Flag, HeightData, HeightSrc, RelativePath, Text as T, WidthData, WidthSrc};

const CROSSORIGIN: AttrSeed = ("crossorigin", E, &["anonymous", "use-credentials"]);
const REFERRERPOLICY: AttrSeed = ("referrerpolicy", E, &["no-referrer", "no-referrer-when-downgrade", "same-origin", "origin", "strict-origin", "origin-when-cross-origin", "strict-origin-when-cross-origin", "unsafe-url"]);
const TARGET: AttrSeed = ("target", E, &["_blank", "_self", "_parent", "_top"]);
const LOADING: AttrSeed = ("loading", E, &["lazy", "eager"]);
const FORM: AttrSeed = ("form", CssId, &[]);
const NAME: AttrSeed = ("name", T, &[]);
const DISABLED: AttrSeed = ("disabled", Flag, &[]);
const AUTOCOMPLETE: AttrSeed = ("autocomplete", T, &[]);
const MEDIA_FLAGS: [AttrSeed; 7] = [("src", RelativePath, &[]), ("controls", Flag, &[]), ("autoplay", Flag, &[]), ("loop", Flag, &[]), ("muted", Flag, &[]), ("preload", E, &["none", "metadata", "auto"]), CROSSORIGIN];

/// The global attributes of the HTML standard (cross-tag group `html_global`).
const GLOBAL_ATTRIBUTES: &[AttrSeed] = &[
    ("accesskey", T, &[]),
    ("autocapitalize", E, &["off", "none", "on", "sentences", "words", "characters"]),
    ("autofocus", Flag, &[]),
    ("class", CssClass, &[]),
    ("contenteditable", E, &["true", "false", "plaintext-only"]),
    ("dir", E, &["ltr", "rtl", "auto"]),
    ("draggable", E, &["true", "false"]),
    ("enterkeyhint", E, &["enter", "done", "go", "next", "previous", "search", "send"]),
    ("hidden", Flag, &[]),
    ("id", CssId, &[]),
    ("inert", Flag, &[]),
    ("inputmode", E, &["none", "text", "decimal", "numeric", "tel", "search", "email", "url"]),
    ("is", T, &[]),
    ("itemid", T, &[]),
    ("itemprop", T, &[]),
    ("itemref", T, &[]),
    ("itemscope", Flag, &[]),
    ("itemtype", T, &[]),
    ("lang", T, &[]),
    ("nonce", T, &[]),
    ("popover", E, &["auto", "manual"]),
    ("slot", T, &[]),
    ("spellcheck", E, &["true", "false"]),
    ("style", CssStyle, &[]),
    ("tabindex", T, &[]),
    ("title", T, &[]),
    ("translate", E, &["yes", "no"]),
];

/// Event handler content attributes (cross-tag group `html_events`; a subset of the standard's list).
const EVENT_HANDLERS: &[&str] = &[
    "onabort", "onblur", "onchange", "onclick", "oncontextmenu", "ondblclick", "onerror", "onfocus", "oninput", "oninvalid", "onkeydown", "onkeypress", "onkeyup", "onload", "onmousedown", "onmouseenter", "onmouseleave", "onmousemove",
    "onmouseout", "onmouseover", "onmouseup", "onreset", "onresize", "onscroll", "onselect", "onsubmit", "onunload",
];

const INPUT_TYPES: &[&str] = &["hidden", "text", "search", "tel", "url", "email", "password", "date", "month", "week", "time", "datetime-local", "number", "range", "color", "checkbox", "radio", "file", "submit", "image", "reset", "button"];

/// The HTML elements (module docs): name, formatting class, void, own attributes.
const HTML_TAGS: &[TagSeed] = &[
    TagSeed("a", Fmt::Inline, false, &[("href", RelativePath, &[]), TARGET, ("download", T, &[]), ("ping", T, &[]), ("rel", T, &[]), ("hreflang", T, &[]), ("type", T, &[]), REFERRERPOLICY]),
    TagSeed("abbr", Fmt::Inline, false, &[]),
    TagSeed("address", Fmt::Container, false, &[]),
    TagSeed("area", Fmt::Block, true, &[("alt", T, &[]), ("coords", T, &[]), ("shape", E, &["rect", "circle", "poly", "default"]), ("href", RelativePath, &[]), TARGET, ("download", T, &[]), ("ping", T, &[]), ("rel", T, &[]), REFERRERPOLICY]),
    TagSeed("article", Fmt::Container, false, &[]),
    TagSeed("aside", Fmt::Container, false, &[]),
    TagSeed("audio", Fmt::Inline, false, &MEDIA_FLAGS),
    TagSeed("b", Fmt::Inline, false, &[]),
    TagSeed("base", Fmt::Block, true, &[("href", RelativePath, &[]), TARGET]),
    TagSeed("bdi", Fmt::Inline, false, &[]),
    TagSeed("bdo", Fmt::Inline, false, &[]),
    TagSeed("blockquote", Fmt::Container, false, &[("cite", T, &[])]),
    TagSeed("body", Fmt::Container, false, &[]),
    TagSeed("br", Fmt::Inline, true, &[]),
    TagSeed(
        "button",
        Fmt::Inline,
        false,
        &[DISABLED, FORM, ("formaction", RelativePath, &[]), ("formenctype", E, &["application/x-www-form-urlencoded", "multipart/form-data", "text/plain"]), ("formmethod", E, &["get", "post", "dialog"]), ("formnovalidate", Flag, &[]), ("formtarget", T, &[]), NAME, ("popovertarget", CssId, &[]), ("popovertargetaction", E, &["toggle", "show", "hide"]), ("type", E, &["submit", "reset", "button"]), ("value", T, &[])],
    ),
    TagSeed("canvas", Fmt::Inline, false, &[("width", T, &[]), ("height", T, &[])]),
    TagSeed("caption", Fmt::Block, false, &[]),
    TagSeed("cite", Fmt::Inline, false, &[]),
    TagSeed("code", Fmt::Inline, false, &[]),
    TagSeed("col", Fmt::Block, true, &[("span", T, &[])]),
    TagSeed("colgroup", Fmt::Container, false, &[("span", T, &[])]),
    TagSeed("data", Fmt::Inline, false, &[("value", T, &[])]),
    TagSeed("datalist", Fmt::Container, false, &[]),
    TagSeed("dd", Fmt::Block, false, &[]),
    TagSeed("del", Fmt::Inline, false, &[("cite", T, &[]), ("datetime", T, &[])]),
    TagSeed("details", Fmt::Container, false, &[("name", T, &[]), ("open", Flag, &[])]),
    TagSeed("dfn", Fmt::Inline, false, &[]),
    TagSeed("dialog", Fmt::Container, false, &[("open", Flag, &[])]),
    TagSeed("div", Fmt::Container, false, &[]),
    TagSeed("dl", Fmt::Container, false, &[]),
    TagSeed("dt", Fmt::Block, false, &[]),
    TagSeed("em", Fmt::Inline, false, &[]),
    TagSeed("embed", Fmt::Inline, true, &[("src", RelativePath, &[]), ("type", T, &[]), ("width", WidthSrc, &[]), ("height", HeightSrc, &[])]),
    TagSeed("fieldset", Fmt::Container, false, &[DISABLED, FORM, NAME]),
    TagSeed("figcaption", Fmt::Block, false, &[]),
    TagSeed("figure", Fmt::Container, false, &[]),
    TagSeed("footer", Fmt::Container, false, &[]),
    TagSeed(
        "form",
        Fmt::Container,
        false,
        &[("accept-charset", T, &[]), ("action", RelativePath, &[]), ("autocomplete", E, &["on", "off"]), ("enctype", E, &["application/x-www-form-urlencoded", "multipart/form-data", "text/plain"]), ("method", E, &["get", "post", "dialog"]), NAME, ("novalidate", Flag, &[]), ("rel", T, &[]), TARGET],
    ),
    TagSeed("h1", Fmt::Block, false, &[]),
    TagSeed("h2", Fmt::Block, false, &[]),
    TagSeed("h3", Fmt::Block, false, &[]),
    TagSeed("h4", Fmt::Block, false, &[]),
    TagSeed("h5", Fmt::Block, false, &[]),
    TagSeed("h6", Fmt::Block, false, &[]),
    TagSeed("head", Fmt::Container, false, &[]),
    TagSeed("header", Fmt::Container, false, &[]),
    TagSeed("hgroup", Fmt::Container, false, &[]),
    TagSeed("hr", Fmt::Block, true, &[]),
    TagSeed("html", Fmt::Root, false, &[]),
    TagSeed("i", Fmt::Inline, false, &[]),
    TagSeed("iframe", Fmt::VerbatimInline, false, &[("src", RelativePath, &[]), ("srcdoc", T, &[]), NAME, ("sandbox", T, &[]), ("allow", T, &[]), ("allowfullscreen", Flag, &[]), ("width", T, &[]), ("height", T, &[]), REFERRERPOLICY, LOADING]),
    TagSeed(
        "img",
        Fmt::Inline,
        true,
        &[("alt", T, &[]), ("src", RelativePath, &[]), ("srcset", T, &[]), ("sizes", T, &[]), CROSSORIGIN, ("usemap", T, &[]), ("ismap", Flag, &[]), ("width", WidthSrc, &[]), ("height", HeightSrc, &[]), REFERRERPOLICY, ("decoding", E, &["sync", "async", "auto"]), LOADING, ("fetchpriority", E, &["high", "low", "auto"])],
    ),
    TagSeed(
        "input",
        Fmt::Inline,
        true,
        &[
            ("accept", T, &[]),
            ("alt", T, &[]),
            AUTOCOMPLETE,
            ("checked", Flag, &[]),
            ("dirname", T, &[]),
            DISABLED,
            FORM,
            ("formaction", RelativePath, &[]),
            ("formmethod", E, &["get", "post", "dialog"]),
            ("formnovalidate", Flag, &[]),
            ("formtarget", T, &[]),
            ("height", HeightSrc, &[]),
            ("list", CssId, &[]),
            ("max", T, &[]),
            ("maxlength", T, &[]),
            ("min", T, &[]),
            ("minlength", T, &[]),
            ("multiple", Flag, &[]),
            NAME,
            ("pattern", T, &[]),
            ("placeholder", T, &[]),
            ("readonly", Flag, &[]),
            ("required", Flag, &[]),
            ("size", T, &[]),
            ("src", RelativePath, &[]),
            ("step", T, &[]),
            ("type", E, INPUT_TYPES),
            ("value", T, &[]),
            ("width", WidthSrc, &[]),
        ],
    ),
    TagSeed("ins", Fmt::Inline, false, &[("cite", T, &[]), ("datetime", T, &[])]),
    TagSeed("kbd", Fmt::Inline, false, &[]),
    TagSeed("label", Fmt::Inline, false, &[("for", CssId, &[])]),
    TagSeed("legend", Fmt::Block, false, &[]),
    TagSeed("li", Fmt::Block, false, &[("value", T, &[])]),
    TagSeed(
        "link",
        Fmt::Block,
        true,
        &[("href", RelativePath, &[]), CROSSORIGIN, ("rel", T, &[]), ("as", T, &[]), ("media", T, &[]), ("integrity", T, &[]), ("hreflang", T, &[]), ("type", T, &[]), REFERRERPOLICY, ("sizes", T, &[]), ("imagesrcset", T, &[]), ("imagesizes", T, &[]), ("blocking", T, &[]), ("color", ValueType::Color, &[]), ("fetchpriority", E, &["high", "low", "auto"])],
    ),
    TagSeed("main", Fmt::Container, false, &[]),
    TagSeed("map", Fmt::Inline, false, &[NAME]),
    TagSeed("mark", Fmt::Inline, false, &[]),
    TagSeed("menu", Fmt::Container, false, &[]),
    TagSeed("meta", Fmt::Block, true, &[NAME, ("http-equiv", E, &["content-language", "content-type", "default-style", "refresh", "set-cookie", "x-ua-compatible", "content-security-policy"]), ("content", T, &[]), ("charset", T, &[]), ("media", T, &[])]),
    TagSeed("meter", Fmt::Inline, false, &[("value", T, &[]), ("min", T, &[]), ("max", T, &[]), ("low", T, &[]), ("high", T, &[]), ("optimum", T, &[])]),
    TagSeed("nav", Fmt::Container, false, &[]),
    TagSeed("noscript", Fmt::Verbatim, false, &[]),
    TagSeed("object", Fmt::Inline, false, &[("data", RelativePath, &[]), ("type", T, &[]), NAME, FORM, ("width", WidthData, &[]), ("height", HeightData, &[])]),
    TagSeed("ol", Fmt::Container, false, &[("reversed", Flag, &[]), ("start", T, &[]), ("type", E, &["1", "a", "A", "i", "I"])]),
    TagSeed("optgroup", Fmt::Container, false, &[DISABLED, ("label", T, &[])]),
    TagSeed("option", Fmt::Block, false, &[DISABLED, ("label", T, &[]), ("selected", Flag, &[]), ("value", T, &[])]),
    TagSeed("output", Fmt::Inline, false, &[("for", T, &[]), FORM, NAME]),
    TagSeed("p", Fmt::Block, false, &[]),
    TagSeed("picture", Fmt::Inline, false, &[]),
    TagSeed("pre", Fmt::Verbatim, false, &[]),
    TagSeed("progress", Fmt::Inline, false, &[("value", T, &[]), ("max", T, &[])]),
    TagSeed("q", Fmt::Inline, false, &[("cite", T, &[])]),
    TagSeed("rp", Fmt::Inline, false, &[]),
    TagSeed("rt", Fmt::Inline, false, &[]),
    TagSeed("ruby", Fmt::Inline, false, &[]),
    TagSeed("s", Fmt::Inline, false, &[]),
    TagSeed("samp", Fmt::Inline, false, &[]),
    TagSeed(
        "script",
        Fmt::Verbatim,
        false,
        &[("src", RelativePath, &[]), ("type", T, &[]), ("nomodule", Flag, &[]), ("async", Flag, &[]), ("defer", Flag, &[]), CROSSORIGIN, ("integrity", T, &[]), REFERRERPOLICY, ("blocking", T, &[]), ("fetchpriority", E, &["high", "low", "auto"])],
    ),
    TagSeed("search", Fmt::Container, false, &[]),
    TagSeed("section", Fmt::Container, false, &[]),
    TagSeed("select", Fmt::Inline, false, &[AUTOCOMPLETE, DISABLED, FORM, ("multiple", Flag, &[]), NAME, ("required", Flag, &[]), ("size", T, &[])]),
    TagSeed("slot", Fmt::Inline, false, &[NAME]),
    TagSeed("small", Fmt::Inline, false, &[]),
    TagSeed("source", Fmt::Block, true, &[("type", T, &[]), ("media", T, &[]), ("src", RelativePath, &[]), ("srcset", T, &[]), ("sizes", T, &[]), ("width", WidthSrc, &[]), ("height", HeightSrc, &[])]),
    TagSeed("span", Fmt::Inline, false, &[]),
    TagSeed("strong", Fmt::Inline, false, &[]),
    TagSeed("style", Fmt::Verbatim, false, &[("media", T, &[]), ("blocking", T, &[])]),
    TagSeed("sub", Fmt::Inline, false, &[]),
    TagSeed("summary", Fmt::Block, false, &[]),
    TagSeed("sup", Fmt::Inline, false, &[]),
    TagSeed("table", Fmt::Container, false, &[]),
    TagSeed("tbody", Fmt::Container, false, &[]),
    TagSeed("td", Fmt::Block, false, &[("colspan", T, &[]), ("rowspan", T, &[]), ("headers", T, &[])]),
    TagSeed("template", Fmt::Container, false, &[("shadowrootmode", E, &["open", "closed"]), ("shadowrootdelegatesfocus", Flag, &[]), ("shadowrootclonable", Flag, &[]), ("shadowrootserializable", Flag, &[])]),
    TagSeed(
        "textarea",
        Fmt::VerbatimInline,
        false,
        &[AUTOCOMPLETE, ("cols", T, &[]), ("dirname", T, &[]), DISABLED, FORM, ("maxlength", T, &[]), ("minlength", T, &[]), NAME, ("placeholder", T, &[]), ("readonly", Flag, &[]), ("required", Flag, &[]), ("rows", T, &[]), ("wrap", E, &["soft", "hard"])],
    ),
    TagSeed("tfoot", Fmt::Container, false, &[]),
    TagSeed("th", Fmt::Block, false, &[("colspan", T, &[]), ("rowspan", T, &[]), ("headers", T, &[]), ("scope", E, &["row", "col", "rowgroup", "colgroup"]), ("abbr", T, &[])]),
    TagSeed("thead", Fmt::Container, false, &[]),
    TagSeed("time", Fmt::Inline, false, &[("datetime", T, &[])]),
    TagSeed("title", Fmt::Block, false, &[]),
    TagSeed("tr", Fmt::Container, false, &[]),
    TagSeed("track", Fmt::Block, true, &[("kind", E, &["subtitles", "captions", "descriptions", "chapters", "metadata"]), ("src", RelativePath, &[]), ("srclang", T, &[]), ("label", T, &[]), ("default", Flag, &[])]),
    TagSeed("u", Fmt::Inline, false, &[]),
    TagSeed("ul", Fmt::Container, false, &[]),
    TagSeed("var", Fmt::Inline, false, &[]),
    TagSeed("video", Fmt::Inline, false, &[("src", RelativePath, &[]), ("controls", Flag, &[]), ("autoplay", Flag, &[]), ("loop", Flag, &[]), ("muted", Flag, &[]), ("preload", E, &["none", "metadata", "auto"]), CROSSORIGIN, ("poster", RelativePath, &[]), ("playsinline", Flag, &[]), ("width", T, &[]), ("height", T, &[])]),
    TagSeed("wbr", Fmt::Inline, true, &[]),
];
