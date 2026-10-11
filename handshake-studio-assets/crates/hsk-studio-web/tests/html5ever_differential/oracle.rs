//! Reference reading: html5ever 0.40.1 (servo, MIT OR Apache-2.0) tree builder into a minimal reference-counted tree.
//!
//! The sink follows the shape of the `arena` example shipped with html5ever (TreeSink with text merging, template
//! contents and the MathML annotation-xml integration-point flag); `oracle_sink_self_check` pins its behaviour so a sink
//! bug cannot masquerade as a product disagreement.

use crate::events::{Ev, Events, Ns};
use html5ever::interface::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{
    Attribute, LocalName, Namespace, ParseOpts, QualName, parse_document, parse_fragment,
};
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

const HTML_NS: &str = "http://www.w3.org/1999/xhtml";
const SVG_NS: &str = "http://www.w3.org/2000/svg";
const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";

pub enum Data {
    Document,
    Doctype,
    Comment(String),
    Text(RefCell<String>),
    Pi,
    Element {
        name: QualName,
        attrs: RefCell<Vec<Attribute>>,
        template: Option<Handle>,
        annotation_ip: bool,
    },
}

pub struct Node {
    data: Data,
    parent: RefCell<Option<Weak<Node>>>,
    children: RefCell<Vec<Handle>>,
}

pub type Handle = Rc<Node>;

fn new_node(data: Data) -> Handle {
    Rc::new(Node {
        data,
        parent: RefCell::new(None),
        children: RefCell::new(Vec::new()),
    })
}

fn detach(node: &Handle) {
    let parent = node.parent.borrow_mut().take().and_then(|w| w.upgrade());
    if let Some(parent) = parent {
        parent
            .children
            .borrow_mut()
            .retain(|c| !Rc::ptr_eq(c, node));
    }
}

/// Place `child` under `parent` at `index` (`None` = end). Text merges into a preceding text node.
fn place(parent: &Handle, index: Option<usize>, child: NodeOrText<Handle>) {
    let mut children = parent.children.borrow_mut();
    let at = index.unwrap_or(children.len()).min(children.len());
    match child {
        NodeOrText::AppendText(text) => {
            if at > 0
                && let Data::Text(existing) = &children[at - 1].data
            {
                existing.borrow_mut().push_str(&text);
                return;
            }
            let node = new_node(Data::Text(RefCell::new(text.to_string())));
            *node.parent.borrow_mut() = Some(Rc::downgrade(parent));
            children.insert(at, node);
        }
        NodeOrText::AppendNode(node) => {
            *node.parent.borrow_mut() = Some(Rc::downgrade(parent));
            children.insert(at, node);
        }
    }
}

pub struct Sink {
    document: Handle,
}

impl Sink {
    fn new() -> Self {
        Self {
            document: new_node(Data::Document),
        }
    }
}

impl TreeSink for Sink {
    type Handle = Handle;
    type Output = Handle;
    type ElemName<'a>
        = &'a QualName
    where
        Self: 'a;

    fn finish(self) -> Handle {
        self.document
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Handle {
        self.document.clone()
    }

    fn set_quirks_mode(&self, _mode: QuirksMode) {}

    fn same_node(&self, x: &Handle, y: &Handle) -> bool {
        Rc::ptr_eq(x, y)
    }

    fn elem_name<'a>(&'a self, target: &'a Handle) -> &'a QualName {
        match &target.data {
            Data::Element { name, .. } => name,
            _ => panic!("elem_name on a non-element"),
        }
    }

    fn get_template_contents(&self, target: &Handle) -> Handle {
        match &target.data {
            Data::Element {
                template: Some(contents),
                ..
            } => contents.clone(),
            _ => panic!("get_template_contents on a non-template"),
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, target: &Handle) -> bool {
        match &target.data {
            Data::Element { annotation_ip, .. } => *annotation_ip,
            _ => panic!("integration point query on a non-element"),
        }
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        new_node(Data::Element {
            name,
            attrs: RefCell::new(attrs),
            template: flags.template.then(|| new_node(Data::Document)),
            annotation_ip: flags.mathml_annotation_xml_integration_point,
        })
    }

    fn create_comment(&self, text: StrTendril) -> Handle {
        new_node(Data::Comment(text.to_string()))
    }

    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> Handle {
        new_node(Data::Pi)
    }

    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        if let NodeOrText::AppendNode(node) = &child {
            detach(node);
        }
        place(parent, None, child);
    }

    fn append_before_sibling(&self, sibling: &Handle, child: NodeOrText<Handle>) {
        if let NodeOrText::AppendNode(node) = &child {
            detach(node);
        }
        let parent = sibling
            .parent
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .expect("sibling has a parent");
        let index = parent
            .children
            .borrow()
            .iter()
            .position(|c| Rc::ptr_eq(c, sibling))
            .expect("sibling is a child of its parent");
        place(&parent, Some(index), child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        prev_element: &Handle,
        child: NodeOrText<Handle>,
    ) {
        if element
            .parent
            .borrow()
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some()
        {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        _name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
        let doctype = new_node(Data::Doctype);
        place(&self.document, None, NodeOrText::AppendNode(doctype));
    }

    fn add_attrs_if_missing(&self, target: &Handle, attrs: Vec<Attribute>) {
        let Data::Element {
            attrs: existing, ..
        } = &target.data
        else {
            panic!("add_attrs_if_missing on a non-element")
        };
        let mut existing = existing.borrow_mut();
        for attr in attrs {
            if !existing.iter().any(|e| e.name == attr.name) {
                existing.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &Handle) {
        detach(target);
    }

    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let moved: Vec<Handle> = std::mem::take(&mut *node.children.borrow_mut());
        for child in moved {
            *child.parent.borrow_mut() = None;
            place(new_parent, None, NodeOrText::AppendNode(child));
        }
    }
}

/// Parse a fragment with a `div` context element (HTML Standard 13.4: the fragment parsing algorithm) or a whole
/// document. `scripting` is the tree builder's scripting flag (browsers: true).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Fragment,
    Document,
}

pub fn parse_reference(input: &str, mode: Mode, scripting: bool) -> Handle {
    let mut opts = ParseOpts::default();
    opts.tree_builder.scripting_enabled = scripting;
    match mode {
        Mode::Fragment => {
            let context = QualName::new(None, Namespace::from(HTML_NS), LocalName::from("div"));
            parse_fragment(Sink::new(), opts, context, Vec::new(), false).one(input)
        }
        Mode::Document => parse_document(Sink::new(), opts).one(input),
    }
}

fn ns_of(name: &QualName) -> Ns {
    match &*name.ns {
        HTML_NS => Ns::Html,
        SVG_NS => Ns::Svg,
        MATHML_NS => Ns::MathMl,
        other => panic!("unexpected element namespace {other:?}"),
    }
}

fn walk(node: &Handle, out: &mut Events) {
    match &node.data {
        Data::Element {
            name,
            attrs,
            template,
            ..
        } => {
            let attrs = attrs
                .borrow()
                .iter()
                .map(|a| {
                    let local = match &a.name.prefix {
                        Some(prefix) if !prefix.is_empty() => {
                            format!("{}:{}", &**prefix, &*a.name.local)
                        }
                        _ => a.name.local.to_string(),
                    };
                    (local.to_ascii_lowercase(), a.value.to_string())
                })
                .collect();
            out.push(Ev::Open {
                ns: ns_of(name),
                name: name.local.to_string().to_ascii_lowercase(),
                attrs,
            });
            let children = template.as_ref().unwrap_or(node);
            for child in children.children.borrow().iter() {
                walk(child, out);
            }
            out.push(Ev::Close);
        }
        Data::Text(t) => out.push(Ev::Text(t.borrow().clone())),
        Data::Comment(c) => out.push(Ev::Comment(c.clone())),
        Data::Document => {
            for child in node.children.borrow().iter() {
                walk(child, out);
            }
        }
        Data::Doctype | Data::Pi => {}
    }
}

/// Events of the parsed result: a fragment's nodes (below the fragment root `html`) or the whole document.
pub fn reference_events(input: &str, mode: Mode, scripting: bool) -> Vec<Ev> {
    let document = parse_reference(input, mode, scripting);
    let mut out = Events::default();
    match mode {
        Mode::Fragment => {
            let root = document
                .children
                .borrow()
                .first()
                .cloned()
                .expect("fragment parsing yields a root element");
            for child in root.children.borrow().iter() {
                walk(child, &mut out);
            }
        }
        Mode::Document => walk(&document, &mut out),
    }
    out.0
}
