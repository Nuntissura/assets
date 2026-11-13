//! Lane SMB-WEB-CODE-1: [STU-WEB-011] to [STU-WEB-016] (document types, doctype declarations, MIME map).
use hsk_studio_web_core::doc_types::{DOCTYPES, DOCUMENT_TYPES, DtdContext, InternalKind, MIME_MAP, doctype, document_type, mime_for_extension, types_for_file_name};
use hsk_studio_web_core::sha256_hex;

#[test]
fn the_document_type_set_is_the_spec_table() {
    assert_eq!(DOCUMENT_TYPES.len(), 39);
    let ids: Vec<&str> = DOCUMENT_TYPES.iter().map(|t| t.type_id).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 39, "type ids are unique");
    // Four rows write a byte-order mark, thirty-five do not.
    assert_eq!(DOCUMENT_TYPES.iter().filter(|t| t.writes_byte_order_mark).count(), 4);
    let html = document_type("html").unwrap();
    assert_eq!((html.internal_kind, html.mime_type, html.dtd_context), (InternalKind::Html, "text/html", Some(DtdContext::Html)));
    assert_eq!(html.windows_extensions.first(), Some(&"html"), "the first extension is the default");
    assert_eq!(html.windows_extensions, html.mac_extensions);
    let php = document_type("php").unwrap();
    assert_eq!((php.internal_kind, php.server_model_id), (InternalKind::Dynamic, Some("php_mysql")));
    assert_eq!(document_type("asp_vb").unwrap().server_model_id, Some("asp_vbs"));
    assert_eq!(document_type("css").unwrap().dtd_context, None, "the table's dash is no context");
    assert_eq!(document_type("edml").unwrap().dtd_context, Some(DtdContext::None), "the member none");
    assert_eq!(DOCUMENT_TYPES.iter().filter(|t| t.is_template()).count(), 8, "rows 23 and 33 to 39");
    assert!(document_type("svg").unwrap().xml_family() && !document_type("php").unwrap().xml_family());
    assert!(document_type("HTML").is_none());
    // Every DYNAMIC row carries a server model ([STU-WEB-012]).
    assert!(DOCUMENT_TYPES.iter().filter(|t| t.internal_kind == InternalKind::Dynamic).all(|t| t.server_model_id.is_some()));
}

#[test]
fn kind_and_dtd_context_are_closed() {
    assert_eq!(InternalKind::ALL.map(InternalKind::token), ["HTML", "TEXT", "XML", "XSLT", "DYNAMIC", "TEMPLATE"]);
    assert_eq!(InternalKind::parse("TEMPLATE"), Some(InternalKind::Template));
    assert_eq!(InternalKind::parse("template"), None);
    assert_eq!(InternalKind::parse("JSON"), None);
    assert_eq!(DtdContext::ALL.len(), 7);
    assert_eq!(DtdContext::parse("jsp_tag_library"), Some(DtdContext::JspTagLibrary));
    assert_eq!(DtdContext::parse("xhtml"), None);
}

#[test]
fn the_ten_doctypes_are_byte_exact() {
    assert_eq!(DOCTYPES.len(), 10);
    for d in &DOCTYPES {
        assert_eq!(sha256_hex(d.literal.as_bytes()), d.sha256, "{}", d.declaration_id);
    }
    assert_eq!(doctype("html_5").unwrap().literal, "<!doctype html>");
    assert!(doctype("xslt_1").unwrap().literal.ends_with("]>\n\t\t\t"), "XSLT's trailing bytes are significant");
    assert!(doctype("html_6").is_none());
}

#[test]
fn file_names_resolve_by_the_longest_extension() {
    let ids = |name: &str| types_for_file_name(name).iter().map(|t| t.type_id).collect::<Vec<_>>();
    assert_eq!(ids("index.HTML"), ["html"]);
    assert_eq!(ids("site.dwt.asp"), ["template_asp_vb", "template_asp_js"]);
    assert_eq!(ids("page.asp"), ["asp_js", "asp_vb"]);
    assert_eq!(ids("t.xsl"), ["xslt_page", "xslt_fragment"]);
    assert!(ids("archive.zip").is_empty());
    assert!(ids(".html").is_empty(), "no base name");
    assert!(ids(&format!("{}.html", "a".repeat(400))).is_empty(), "bounded");
}

#[test]
fn the_mime_map_has_thirteen_rows_and_a_reported_fallback() {
    assert_eq!(MIME_MAP.len(), 13);
    assert_eq!(mime_for_extension("ICO"), ("image/x-icon", true));
    assert_eq!(mime_for_extension("ttf"), ("font/ttf", true));
    assert_eq!(mime_for_extension("webp"), ("application/octet-stream", false));
}

/// Review F14: the 39 rows against a literal copy of the spec table (STU-WEB-013): id, kind, extensions, MIME, BOM,
/// DTD context and server model.
#[test]
fn the_39_rows_match_the_spec_table() {
    #[allow(clippy::type_complexity)]
    let spec: [(&str, &str, &str, &str, bool, Option<&str>, Option<&str>); 39] = [
    ("html", "HTML", "html,htm,shtml,shtm,stm,tpl,lasso,xhtml", "text/html", false, Some("html"), None),
    ("css", "TEXT", "css", "text/css", false, None, None),
    ("less", "TEXT", "less", "text/css", false, None, None),
    ("scss", "TEXT", "scss", "text/css", false, None, None),
    ("sass", "TEXT", "sass", "text/css", false, None, None),
    ("javascript", "TEXT", "js", "text/javascript", false, None, None),
    ("json", "TEXT", "json", "application/json", false, None, None),
    ("xml", "XML", "xml,xsd,rss,rdf,dtd,vtm,vtml,csn,config,mxi", "text/xml", false, Some("xml"), None),
    ("svg", "XML", "svg", "image/svg+xml", false, Some("svg"), None),
    ("text", "TEXT", "txt", "text/plain", false, None, None),
    ("wml", "XML", "wml", "text/xml", false, Some("wml"), None),
    ("tld", "XML", "tld", "text/xml", false, Some("jsp_tag_library"), None),
    ("edml", "XML", "edml,edm", "text/xml", false, Some("none"), None),
    ("vbscript", "TEXT", "vbs", "text/vbscript", false, None, None),
    ("csharp", "TEXT", "cs", "text/cs", false, None, None),
    ("vb", "TEXT", "vb", "text/vb", false, None, None),
    ("java", "TEXT", "java", "text/java", false, None, None),
    ("actionscript", "TEXT", "as", "text/as", false, None, None),
    ("actionscript_comm", "TEXT", "asc", "text/asc", false, None, None),
    ("actionscript_remote", "TEXT", "asr", "text/asr", false, None, None),
    ("xslt_page", "XSLT", "xsl,xslt", "text/xsl", false, Some("xslt"), Some("xslt")),
    ("xslt_fragment", "XSLT", "xsl,xslt", "text/xsl", false, Some("none"), Some("xslt")),
    ("template_html", "TEMPLATE", "dwt", "text/html", false, Some("html"), None),
    ("library_item", "HTML", "lbi", "text/html", false, Some("none"), None),
    ("php", "DYNAMIC", "php,php3,php4,php5,phtml", "text/html", false, Some("html"), Some("php_mysql")),
    ("asp_js", "DYNAMIC", "asp", "text/html", false, Some("html"), Some("asp_js")),
    ("asp_vb", "DYNAMIC", "asp", "text/html", false, Some("html"), Some("asp_vbs")),
    ("coldfusion", "DYNAMIC", "cfm,cfml", "text/html", false, Some("html"), Some("coldfusion")),
    ("coldfusion_component", "DYNAMIC", "cfc", "text/html", false, Some("none"), Some("coldfusion")),
    ("jsp", "DYNAMIC", "jsp,jst", "text/html", false, Some("html"), Some("jsp")),
    ("aspnet_vb", "DYNAMIC", "aspx,ascx,asmx", "text/html", true, Some("html"), Some("aspnet_vb")),
    ("aspnet_csharp", "DYNAMIC", "aspx,ascx,asmx", "text/html", true, Some("html"), Some("aspnet_csharp")),
    ("template_asp_vb", "TEMPLATE", "dwt.asp", "text/html", false, Some("html"), None),
    ("template_asp_js", "TEMPLATE", "dwt.asp", "text/html", false, Some("html"), None),
    ("template_coldfusion", "TEMPLATE", "dwt.cfm", "text/html", false, Some("html"), None),
    ("template_jsp", "TEMPLATE", "dwt.jsp", "text/html", false, Some("html"), None),
    ("template_aspnet_csharp", "TEMPLATE", "dwt.aspx", "text/html", true, Some("html"), None),
    ("template_aspnet_vb", "TEMPLATE", "dwt.aspx", "text/html", true, Some("html"), None),
    ("template_php", "TEMPLATE", "dwt.php", "text/html", false, Some("html"), None),
    ];
    for (row, (id, kind, exts, mime, bom, dtd, server)) in DOCUMENT_TYPES.iter().zip(spec) {
        assert_eq!(row.type_id, id);
        assert_eq!(row.internal_kind.token(), kind, "{id}");
        assert_eq!(row.windows_extensions.join(","), exts, "{id}");
        assert_eq!((row.mime_type, row.writes_byte_order_mark), (mime, bom), "{id}");
        assert_eq!(row.dtd_context.map(DtdContext::token), dtd, "{id}");
        assert_eq!(row.server_model_id, server, "{id}");
    }
}
