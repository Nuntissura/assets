//! Web document types (lane SMB-WEB-CODE-1; [STU-WEB-011] to [STU-WEB-016], the template rows of [STU-WEB-090]):
//! the document-type record, the closed internal-kind and DTD-context enumerations, the thirty-nine shipped types,
//! the ten doctype declarations with their literal bytes (pinned by the spec's SHA-256 values) and the
//! extension-to-MIME map. Everything here is the spec's own table, nothing from a vendor installation.
//!
//! Readings stated where the spec leaves a choice: a `—` in the DTD-context column is `None` (no context: the TEXT
//! kinds), distinct from the member `none`; `title` and `display` names are the table's; `starter_document_id` is null
//! for every row (the table names none); the Mac extension list equals the Windows list (no row states otherwise).
//! Not here: the per-document byte-order-mark override (it belongs on the document record, which the web host does
//! not keep yet) and the publish receipt's unlisted-extension report (the publish path's).
#![deny(clippy::arithmetic_side_effects)]

use std::ops::Range;

/// A typed refusal of the code-intelligence modules: a stable code and, where one exists, the byte range it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeError {
    pub code: &'static str,
    pub at: Option<Range<usize>>,
}

impl CodeError {
    pub const fn new(code: &'static str) -> Self {
        Self { code, at: None }
    }

    pub const fn at(code: &'static str, at: Range<usize>) -> Self {
        Self { code, at: Some(at) }
    }
}

/// [STU-WEB-012], closed: exactly six members.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InternalKind {
    Html,
    Text,
    Xml,
    Xslt,
    Dynamic,
    Template,
}

impl InternalKind {
    pub const ALL: [Self; 6] = [Self::Html, Self::Text, Self::Xml, Self::Xslt, Self::Dynamic, Self::Template];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Html => "HTML",
            Self::Text => "TEXT",
            Self::Xml => "XML",
            Self::Xslt => "XSLT",
            Self::Dynamic => "DYNAMIC",
            Self::Template => "TEMPLATE",
        }
    }

    /// The member spelled exactly `token`; any other spelling is no member (closed).
    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.token() == token)
    }
}

/// [STU-WEB-014], closed: exactly seven members; it selects the tag libraries and the validator rule set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DtdContext {
    Html,
    Xml,
    Svg,
    Wml,
    Xslt,
    JspTagLibrary,
    None,
}

impl DtdContext {
    pub const ALL: [Self; 7] = [Self::Html, Self::Xml, Self::Svg, Self::Wml, Self::Xslt, Self::JspTagLibrary, Self::None];

    pub const fn token(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Xml => "xml",
            Self::Svg => "svg",
            Self::Wml => "wml",
            Self::Xslt => "xslt",
            Self::JspTagLibrary => "jsp_tag_library",
            Self::None => "none",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.token() == token)
    }
}

/// [STU-WEB-011]: one web document type. Extension arrays are ordered: the first is a new document's default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentType {
    pub type_id: &'static str,
    pub title: &'static str,
    pub internal_kind: InternalKind,
    pub windows_extensions: &'static [&'static str],
    pub mac_extensions: &'static [&'static str],
    pub mime_type: &'static str,
    pub writes_byte_order_mark: bool,
    /// `None` is the table's `—` (no context); `Some(DtdContext::None)` is the member `none`.
    pub dtd_context: Option<DtdContext>,
    pub server_model_id: Option<&'static str>,
    pub starter_document_id: Option<&'static str>,
}

impl DocumentType {
    /// [STU-WEB-025]: tag names match case-sensitively in the XML family, case-insensitively otherwise.
    pub fn xml_family(&self) -> bool {
        matches!(self.type_id, "xml" | "svg" | "xslt_page" | "xslt_fragment" | "wml" | "tld" | "edml")
    }

    /// [STU-WEB-090]: a template document type (rows 23 and 33 to 39).
    pub fn is_template(&self) -> bool {
        self.internal_kind == InternalKind::Template
    }
}

/// One table row (the table's own columns, hence the argument count).
#[allow(clippy::too_many_arguments)]
const fn row(
    type_id: &'static str,
    title: &'static str,
    internal_kind: InternalKind,
    extensions: &'static [&'static str],
    mime_type: &'static str,
    writes_byte_order_mark: bool,
    dtd_context: Option<DtdContext>,
    server_model_id: Option<&'static str>,
) -> DocumentType {
    DocumentType { type_id, title, internal_kind, windows_extensions: extensions, mac_extensions: extensions, mime_type, writes_byte_order_mark, dtd_context, server_model_id, starter_document_id: None }
}

use DtdContext as D;
use InternalKind as K;

/// [STU-WEB-013]: exactly these thirty-nine types, in the table's order.
pub static DOCUMENT_TYPES: [DocumentType; 39] = [
    row("html", "HTML", K::Html, &["html", "htm", "shtml", "shtm", "stm", "tpl", "lasso", "xhtml"], "text/html", false, Some(D::Html), None),
    row("css", "CSS", K::Text, &["css"], "text/css", false, None, None),
    row("less", "LESS", K::Text, &["less"], "text/css", false, None, None),
    row("scss", "SCSS", K::Text, &["scss"], "text/css", false, None, None),
    row("sass", "Sass", K::Text, &["sass"], "text/css", false, None, None),
    row("javascript", "JavaScript", K::Text, &["js"], "text/javascript", false, None, None),
    row("json", "JSON", K::Text, &["json"], "application/json", false, None, None),
    row("xml", "XML", K::Xml, &["xml", "xsd", "rss", "rdf", "dtd", "vtm", "vtml", "csn", "config", "mxi"], "text/xml", false, Some(D::Xml), None),
    row("svg", "SVG", K::Xml, &["svg"], "image/svg+xml", false, Some(D::Svg), None),
    row("text", "Text", K::Text, &["txt"], "text/plain", false, None, None),
    row("wml", "WML", K::Xml, &["wml"], "text/xml", false, Some(D::Wml), None),
    row("tld", "TLD", K::Xml, &["tld"], "text/xml", false, Some(D::JspTagLibrary), None),
    row("edml", "EDML", K::Xml, &["edml", "edm"], "text/xml", false, Some(D::None), None),
    row("vbscript", "VBScript", K::Text, &["vbs"], "text/vbscript", false, None, None),
    row("csharp", "C#", K::Text, &["cs"], "text/cs", false, None, None),
    row("vb", "VB", K::Text, &["vb"], "text/vb", false, None, None),
    row("java", "Java", K::Text, &["java"], "text/java", false, None, None),
    row("actionscript", "ActionScript", K::Text, &["as"], "text/as", false, None, None),
    row("actionscript_comm", "ActionScript communications", K::Text, &["asc"], "text/asc", false, None, None),
    row("actionscript_remote", "ActionScript remote", K::Text, &["asr"], "text/asr", false, None, None),
    row("xslt_page", "XSLT (entire page)", K::Xslt, &["xsl", "xslt"], "text/xsl", false, Some(D::Xslt), Some("xslt")),
    row("xslt_fragment", "XSLT (fragment)", K::Xslt, &["xsl", "xslt"], "text/xsl", false, Some(D::None), Some("xslt")),
    row("template_html", "HTML template", K::Template, &["dwt"], "text/html", false, Some(D::Html), None),
    row("library_item", "Library item", K::Html, &["lbi"], "text/html", false, Some(D::None), None),
    row("php", "PHP", K::Dynamic, &["php", "php3", "php4", "php5", "phtml"], "text/html", false, Some(D::Html), Some("php_mysql")),
    row("asp_js", "ASP JavaScript", K::Dynamic, &["asp"], "text/html", false, Some(D::Html), Some("asp_js")),
    row("asp_vb", "ASP VBScript", K::Dynamic, &["asp"], "text/html", false, Some(D::Html), Some("asp_vbs")),
    row("coldfusion", "ColdFusion", K::Dynamic, &["cfm", "cfml"], "text/html", false, Some(D::Html), Some("coldfusion")),
    row("coldfusion_component", "ColdFusion component", K::Dynamic, &["cfc"], "text/html", false, Some(D::None), Some("coldfusion")),
    row("jsp", "JSP", K::Dynamic, &["jsp", "jst"], "text/html", false, Some(D::Html), Some("jsp")),
    row("aspnet_vb", "ASP.NET VB", K::Dynamic, &["aspx", "ascx", "asmx"], "text/html", true, Some(D::Html), Some("aspnet_vb")),
    row("aspnet_csharp", "ASP.NET C#", K::Dynamic, &["aspx", "ascx", "asmx"], "text/html", true, Some(D::Html), Some("aspnet_csharp")),
    row("template_asp_vb", "ASP VBScript template", K::Template, &["dwt.asp"], "text/html", false, Some(D::Html), None),
    row("template_asp_js", "ASP JavaScript template", K::Template, &["dwt.asp"], "text/html", false, Some(D::Html), None),
    row("template_coldfusion", "ColdFusion template", K::Template, &["dwt.cfm"], "text/html", false, Some(D::Html), None),
    row("template_jsp", "JSP template", K::Template, &["dwt.jsp"], "text/html", false, Some(D::Html), None),
    row("template_aspnet_csharp", "ASP.NET C# template", K::Template, &["dwt.aspx"], "text/html", true, Some(D::Html), None),
    row("template_aspnet_vb", "ASP.NET VB template", K::Template, &["dwt.aspx"], "text/html", true, Some(D::Html), None),
    row("template_php", "PHP template", K::Template, &["dwt.php"], "text/html", false, Some(D::Html), None),
];

/// The type whose `type_id` is exactly `type_id`.
pub fn document_type(type_id: &str) -> Option<&'static DocumentType> {
    DOCUMENT_TYPES.iter().find(|t| t.type_id == type_id)
}

/// Longest file name this module looks at (bounded work on hostile names).
const MAX_FILE_NAME: usize = 255;

/// The types a file name can be, in table order: the longest declared extension the name ends with (`.dwt.asp`
/// over `.asp`) decides, compared ASCII case-insensitively. Several rows share an extension (`asp`, `aspx`, `xsl`):
/// all of them are returned and the caller chooses; an unknown extension is an empty list.
pub fn types_for_file_name(name: &str) -> Vec<&'static DocumentType> {
    if name.len() > MAX_FILE_NAME {
        return Vec::new();
    }
    let lower = name.to_ascii_lowercase();
    // A base name before the dot is required (`.html` alone names no document).
    let ends = |ext: &str| lower.len() > ext.len().saturating_add(1) && lower.ends_with(ext) && lower.as_bytes().get(lower.len().saturating_sub(ext.len()).saturating_sub(1)) == Some(&b'.');
    let best = DOCUMENT_TYPES.iter().flat_map(|t| t.windows_extensions.iter()).filter(|e| ends(e)).map(|e| e.len()).max();
    let Some(best) = best else { return Vec::new() };
    DOCUMENT_TYPES.iter().filter(|t| t.windows_extensions.iter().any(|e| e.len() == best && ends(e))).collect()
}

/// [STU-WEB-015]: one doctype declaration and its literal bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DoctypeDeclaration {
    pub declaration_id: &'static str,
    /// Emitted verbatim (the spec's JSON literal decoded; XSLT's trailing newline and tabs included).
    pub literal: &'static str,
    /// The spec's UTF-8 SHA-256 of `literal`.
    pub sha256: &'static str,
}

const fn decl(declaration_id: &'static str, literal: &'static str, sha256: &'static str) -> DoctypeDeclaration {
    DoctypeDeclaration { declaration_id, literal, sha256 }
}

/// [STU-WEB-015]: exactly these ten declarations.
pub static DOCTYPES: [DoctypeDeclaration; 10] = [
    decl("html_401_transitional", "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\" \"http://www.w3.org/TR/html4/loose.dtd\">", "038bc3463fde72fe84400b49b5d260eb78faece26ad85d7ec9fa7133b3b72669"),
    decl("html_401_strict", "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">", "f746845be5f58de20e676b7bb4aae3438ed8055f48daca7eadf986206da75c8d"),
    decl("html_401_frameset", "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Frameset//EN\" \"http://www.w3.org/TR/html4/frameset.dtd\">", "5c7f995549049033718141e3658f635f3b4bd7ba15fa3b89c90f6eeb10544ae9"),
    decl("html_5", "<!doctype html>", "fe26c59e91ac8de694b2531dc3bdc1b7faf471d3d7e4e00870af60f5f22897cb"),
    decl("xhtml_1_transitional", "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Transitional//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd\">", "7f3c0066fcfae37e040afe3c1f769d0100cff916b62ceaa0f4030d455807ac12"),
    decl("xhtml_1_strict", "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\">", "cf72f33d973cce2de3d1dfd34fc95951a4bb4a866785f18a56e25a56fd484eed"),
    decl("xhtml_11", "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.1//EN\" \"http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd\">", "eda292944b8af7ac65569a2a06741a646b72e62cc68ae15621d536b98f173025"),
    decl("xhtml_1_frameset", "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Frameset//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-frameset.dtd\">", "9b62ccbf25cd4870301dad586c7df6276e14de0ad872758e48aed0053664c404"),
    decl("xhtml_mobile_1", "<!DOCTYPE html PUBLIC \"-//WAPFORUM//DTD XHTML Mobile 1.0//EN\" \"http://www.wapforum.org/DTD/xhtml-mobile10.dtd\">", "3a10e18d5f391ce8ad702429dd6c751e9454de0b63127f200d3242feafa5f29c"),
    decl(
        "xslt_1",
        "<!DOCTYPE xsl:stylesheet  [\n\t<!ENTITY nbsp   \"&#160;\">\n\t<!ENTITY copy   \"&#169;\">\n\t<!ENTITY reg    \"&#174;\">\n\t<!ENTITY trade  \"&#8482;\">\n\t<!ENTITY mdash  \"&#8212;\">\n\t<!ENTITY ldquo  \"&#8220;\">\n\t<!ENTITY rdquo  \"&#8221;\"> \n\t<!ENTITY pound  \"&#163;\">\n\t<!ENTITY yen    \"&#165;\">\n\t<!ENTITY euro   \"&#8364;\">\n]>\n\t\t\t",
        "26aff3d0af763e71c714d330417de2027ec24f02e6e980b22f60fcf7f1f8bbef",
    ),
];

/// [STU-WEB-015]: the default for a new HTML document.
pub const DEFAULT_DOCTYPE: &str = "html_5";

pub fn doctype(declaration_id: &str) -> Option<&'static DoctypeDeclaration> {
    DOCTYPES.iter().find(|d| d.declaration_id == declaration_id)
}

/// [STU-WEB-016]: exactly these thirteen extensions.
pub const MIME_MAP: [(&str, &str); 13] = [
    ("bmp", "image/bmp"),
    ("gif", "image/gif"),
    ("ico", "image/x-icon"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("rss", "application/rss+xml"),
    ("svg", "image/svg+xml"),
    ("swf", "application/x-shockwave-flash"),
    ("tif", "image/tiff"),
    ("tiff", "image/tiff"),
    ("ttf", "font/ttf"),
];

/// The served type of a non-source asset extension and whether the map lists it ([STU-WEB-016]): an unlisted one is
/// `application/octet-stream` with `false`, for the caller to report.
pub fn mime_for_extension(extension: &str) -> (&'static str, bool) {
    MIME_MAP.iter().find(|(e, _)| e.eq_ignore_ascii_case(extension)).map_or(("application/octet-stream", false), |(_, m)| (*m, true))
}
