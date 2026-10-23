//! Canonical source graph, provenance, acquisition, and output sink descriptors.

use crate::{
    AssetId, CompatibilityRange, ContinuationId, DerivedOutputId, EdgeId, ExtensionLimits,
    ExtensionMap, ItemId, NodeId, RepresentationId, SchemaVersion, TrackId, TriState,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

/// Limits applied before a graph enters the domain core.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphLimits {
    pub accepted_schema: CompatibilityRange,
    pub maximum_roots: usize,
    pub maximum_nodes: usize,
    pub maximum_edges: usize,
    pub maximum_continuations: usize,
    pub maximum_identity_values_per_node: usize,
    pub maximum_string_bytes: usize,
    pub maximum_redirect_depth: usize,
    pub maximum_provenance_depth: usize,
    pub extensions: ExtensionLimits,
}

impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            accepted_schema: CompatibilityRange {
                major: 1,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            maximum_roots: 64,
            maximum_nodes: 10_000,
            maximum_edges: 40_000,
            maximum_continuations: 1_000,
            maximum_identity_values_per_node: 4_096,
            maximum_string_bytes: 16 * 1024,
            maximum_redirect_depth: 16,
            maximum_provenance_depth: 64,
            extensions: ExtensionLimits::default(),
        }
    }
}

/// Bounds for acquisition and output-sink descriptors used outside the source graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataContractLimits {
    pub maximum_fragments: usize,
    pub maximum_text_bytes: usize,
    pub maximum_path_reference_bytes: usize,
    pub maximum_buffer_items: u64,
    pub maximum_buffer_bytes: u64,
    pub maximum_sink_lifetime_seconds: u64,
}

impl Default for DataContractLimits {
    fn default() -> Self {
        Self {
            maximum_fragments: 10_000,
            maximum_text_bytes: 16 * 1024,
            maximum_path_reference_bytes: 4 * 1024,
            maximum_buffer_items: 4_096,
            maximum_buffer_bytes: 64 * 1024 * 1024,
            maximum_sink_lifetime_seconds: 31 * 24 * 60 * 60,
        }
    }
}

/// Fail-closed acquisition and sink descriptor validation error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DataContractError {
    LimitExceeded {
        field: &'static str,
        actual: u64,
        maximum: u64,
    },
    EmptyCollection {
        field: &'static str,
    },
    EmptyText {
        field: &'static str,
    },
    InvalidText {
        field: &'static str,
    },
    InvalidPathReference {
        field: &'static str,
        value: String,
    },
    InvalidChecksum {
        sequence: u64,
    },
    DuplicateFragmentSequence {
        sequence: u64,
    },
    InvalidBoundedBuffer,
}

impl fmt::Display for DataContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid data contract: {self:?}")
    }
}

impl std::error::Error for DataContractError {}

/// Canonical, versioned, serializable source graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceGraph {
    pub schema: SchemaVersion,
    pub roots: Vec<NodeId>,
    pub nodes: Vec<SourceNode>,
    pub edges: Vec<SourceEdge>,
    pub continuations: Vec<ContinuationDescriptor>,
}

/// Source-node kind kept stable on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Media,
    Collection,
    Live,
    Redirect,
    MetadataRecord,
    UnsupportedOrProtected,
}

/// Explicit relationship between graph nodes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    Embeds,
    TransparentlyOverlays,
    Alternate,
    Complementary,
    Additional,
    Continuation,
    DerivedOutput,
}

/// Stable identities associated with one node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityIdentities {
    pub item: Option<ItemId>,
    pub representations: Vec<RepresentationId>,
    pub tracks: Vec<TrackId>,
    pub assets: Vec<AssetId>,
    pub derived_outputs: Vec<DerivedOutputId>,
}

/// Origin information preserved across redirects and derivations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub extractor_key: String,
    pub source_identity: String,
    pub canonical_url: String,
    pub parent_node: Option<NodeId>,
}

/// Serializable graph node with no cursor or runtime body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub identities: EntityIdentities,
    pub provenance: Provenance,
    pub title: TriState<String>,
    pub description: TriState<String>,
    pub extensions: ExtensionMap,
}

/// Stable directed edge.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEdge {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
}

/// Lazy traversal descriptor; runtime cursor state is intentionally absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationDescriptor {
    pub id: ContinuationId,
    pub owner: NodeId,
    pub opaque_token: String,
    pub extensions: ExtensionMap,
}

/// Precise graph-validation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphError {
    InvalidSchema {
        received: SchemaVersion,
        accepted: CompatibilityRange,
    },
    LimitExceeded {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },
    DuplicateId {
        kind: &'static str,
        id: String,
    },
    DanglingRoot {
        id: NodeId,
    },
    DanglingEdge {
        edge: EdgeId,
        node: NodeId,
    },
    DanglingContinuation {
        id: ContinuationId,
        node: NodeId,
    },
    DanglingProvenanceParent {
        node: NodeId,
        parent: NodeId,
    },
    ProvenanceCycle {
        node: NodeId,
    },
    ProvenanceBudgetExceeded {
        node: NodeId,
        maximum: usize,
    },
    DuplicateEntityId {
        kind: &'static str,
        id: String,
    },
    StringTooLong {
        field: &'static str,
        node: NodeId,
        actual: usize,
        maximum: usize,
    },
    InvalidString {
        field: &'static str,
        node: NodeId,
    },
    InvalidExtensions {
        owner: String,
    },
    RelationshipCycle {
        node: NodeId,
    },
    RedirectCycle {
        node: NodeId,
    },
    InvalidRedirectSource {
        edge: EdgeId,
        node: NodeId,
    },
    RedirectBudgetExceeded {
        node: NodeId,
        maximum: usize,
    },
    AmbiguousRedirect {
        node: NodeId,
        targets: usize,
    },
    AmbiguousCanonicalSource {
        canonical_url: String,
        source_identity: String,
    },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid source graph: {self:?}")
    }
}
impl std::error::Error for GraphError {}

impl SourceGraph {
    /// Validates all collection, relationship, identity, string, extension, and redirect bounds.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] for a duplicate, dangling relationship, ambiguous
    /// canonical source, cycle, or exceeded configured bound.
    pub fn validate(&self, limits: GraphLimits) -> Result<(), GraphError> {
        validate_graph_schema_and_limits(self, limits)?;
        let nodes = collect_validated_nodes(self, limits)?;
        validate_roots_and_parents(self, &nodes, limits.maximum_provenance_depth)?;
        validate_edges(self, &nodes)?;
        validate_continuations(self, &nodes, limits)?;
        validate_redirects(self, &nodes, limits.maximum_redirect_depth)?;
        validate_relationship_cycles(self)
    }
}

fn validate_graph_schema_and_limits(
    graph: &SourceGraph,
    limits: GraphLimits,
) -> Result<(), GraphError> {
    if limits.accepted_schema.minimum_minor > limits.accepted_schema.maximum_minor
        || limits.accepted_schema.check(graph.schema).is_err()
    {
        return Err(GraphError::InvalidSchema {
            received: graph.schema,
            accepted: limits.accepted_schema,
        });
    }
    check_limit("roots", graph.roots.len(), limits.maximum_roots)?;
    check_limit("nodes", graph.nodes.len(), limits.maximum_nodes)?;
    check_limit("edges", graph.edges.len(), limits.maximum_edges)?;
    check_limit(
        "continuations",
        graph.continuations.len(),
        limits.maximum_continuations,
    )
}

fn collect_validated_nodes(
    graph: &SourceGraph,
    limits: GraphLimits,
) -> Result<BTreeMap<NodeId, &SourceNode>, GraphError> {
    let mut nodes = BTreeMap::new();
    let mut entity_ids = BTreeSet::new();
    let mut canonical = BTreeSet::new();
    for node in &graph.nodes {
        if nodes.insert(node.id.clone(), node).is_some() {
            return Err(GraphError::DuplicateId {
                kind: "node",
                id: node.id.to_string(),
            });
        }
        validate_node(node, limits, &mut entity_ids)?;
        let key = (
            node.provenance.canonical_url.clone(),
            node.provenance.source_identity.clone(),
        );
        if !canonical.insert(key.clone()) {
            return Err(GraphError::AmbiguousCanonicalSource {
                canonical_url: key.0,
                source_identity: key.1,
            });
        }
    }
    Ok(nodes)
}

fn validate_roots_and_parents(
    graph: &SourceGraph,
    nodes: &BTreeMap<NodeId, &SourceNode>,
    maximum_provenance_depth: usize,
) -> Result<(), GraphError> {
    let mut roots = BTreeSet::new();
    for root in &graph.roots {
        if !roots.insert(root.clone()) {
            return Err(GraphError::DuplicateId {
                kind: "root",
                id: root.to_string(),
            });
        }
        if !nodes.contains_key(root) {
            return Err(GraphError::DanglingRoot { id: root.clone() });
        }
    }
    for node in &graph.nodes {
        match &node.provenance.parent_node {
            Some(parent) if !nodes.contains_key(parent) => {
                return Err(GraphError::DanglingProvenanceParent {
                    node: node.id.clone(),
                    parent: parent.clone(),
                });
            }
            _ => {}
        }
    }
    validate_provenance_chains(nodes, maximum_provenance_depth)
}

fn validate_edges(
    graph: &SourceGraph,
    nodes: &BTreeMap<NodeId, &SourceNode>,
) -> Result<(), GraphError> {
    let mut edge_ids = BTreeSet::new();
    for edge in &graph.edges {
        if !edge_ids.insert(edge.id.clone()) {
            return Err(GraphError::DuplicateId {
                kind: "edge",
                id: edge.id.to_string(),
            });
        }
        if !nodes.contains_key(&edge.from) {
            return Err(GraphError::DanglingEdge {
                edge: edge.id.clone(),
                node: edge.from.clone(),
            });
        }
        if !nodes.contains_key(&edge.to) {
            return Err(GraphError::DanglingEdge {
                edge: edge.id.clone(),
                node: edge.to.clone(),
            });
        }
    }
    Ok(())
}

/// Whether an edge participates in recursive source resolution.
///
/// The partition is exhaustive: containment, embedding, transparent overlays, and
/// continuations are traversable and therefore acyclic; alternates, complementary
/// assets, additional assets, and derived outputs are descriptive links only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelationshipTraversal {
    Recursive,
    Descriptive,
}

const fn relationship_traversal(kind: &EdgeKind) -> RelationshipTraversal {
    match kind {
        EdgeKind::Contains
        | EdgeKind::Embeds
        | EdgeKind::TransparentlyOverlays
        | EdgeKind::Continuation => RelationshipTraversal::Recursive,
        EdgeKind::Alternate
        | EdgeKind::Complementary
        | EdgeKind::Additional
        | EdgeKind::DerivedOutput => RelationshipTraversal::Descriptive,
    }
}

/// Traversable relationships must remain acyclic before any recursive resolution.
const fn relationship_cycle_is_forbidden(kind: &EdgeKind) -> bool {
    matches!(
        relationship_traversal(kind),
        RelationshipTraversal::Recursive
    )
}

fn validate_relationship_cycles(graph: &SourceGraph) -> Result<(), GraphError> {
    let mut adjacency = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
    for node in &graph.nodes {
        adjacency.entry(node.id.clone()).or_default();
    }
    for edge in &graph.edges {
        if relationship_cycle_is_forbidden(&edge.kind) {
            adjacency
                .entry(edge.from.clone())
                .or_default()
                .insert(edge.to.clone());
        }
    }

    let roots = adjacency.keys().cloned().collect::<Vec<_>>();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for root in roots {
        if visited.contains(&root) {
            continue;
        }
        let mut stack = vec![(root, false)];
        while let Some((node, exiting)) = stack.pop() {
            if exiting {
                visiting.remove(&node);
                visited.insert(node);
                continue;
            }
            if visited.contains(&node) {
                continue;
            }
            if !visiting.insert(node.clone()) {
                return Err(GraphError::RelationshipCycle { node });
            }
            stack.push((node.clone(), true));
            if let Some(targets) = adjacency.get(&node) {
                for target in targets.iter().rev() {
                    if visiting.contains(target) {
                        return Err(GraphError::RelationshipCycle {
                            node: target.clone(),
                        });
                    }
                    if !visited.contains(target) {
                        stack.push((target.clone(), false));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_continuations(
    graph: &SourceGraph,
    nodes: &BTreeMap<NodeId, &SourceNode>,
    limits: GraphLimits,
) -> Result<(), GraphError> {
    let mut continuation_ids = BTreeSet::new();
    for continuation in &graph.continuations {
        if !continuation_ids.insert(continuation.id.clone()) {
            return Err(GraphError::DuplicateId {
                kind: "continuation",
                id: continuation.id.to_string(),
            });
        }
        if !nodes.contains_key(&continuation.owner) {
            return Err(GraphError::DanglingContinuation {
                id: continuation.id.clone(),
                node: continuation.owner.clone(),
            });
        }
        if continuation.opaque_token.len() > limits.maximum_string_bytes {
            return Err(GraphError::LimitExceeded {
                field: "continuation_token",
                actual: continuation.opaque_token.len(),
                maximum: limits.maximum_string_bytes,
            });
        }
        continuation
            .extensions
            .validate(limits.extensions)
            .map_err(|_| GraphError::InvalidExtensions {
                owner: continuation.id.to_string(),
            })?;
    }
    Ok(())
}

fn check_limit(field: &'static str, actual: usize, maximum: usize) -> Result<(), GraphError> {
    if actual > maximum {
        Err(GraphError::LimitExceeded {
            field,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn validate_node(
    node: &SourceNode,
    limits: GraphLimits,
    entity_ids: &mut BTreeSet<String>,
) -> Result<(), GraphError> {
    for (field, value) in [
        ("extractor_key", &node.provenance.extractor_key),
        ("source_identity", &node.provenance.source_identity),
        ("canonical_url", &node.provenance.canonical_url),
    ] {
        if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(GraphError::InvalidString {
                field,
                node: node.id.clone(),
            });
        }
        if value.len() > limits.maximum_string_bytes {
            return Err(GraphError::StringTooLong {
                field,
                node: node.id.clone(),
                actual: value.len(),
                maximum: limits.maximum_string_bytes,
            });
        }
    }
    let identity_count = [
        usize::from(node.identities.item.is_some()),
        node.identities.representations.len(),
        node.identities.tracks.len(),
        node.identities.assets.len(),
        node.identities.derived_outputs.len(),
    ]
    .into_iter()
    .try_fold(0usize, usize::checked_add)
    .unwrap_or(usize::MAX);
    check_limit(
        "identities_per_node",
        identity_count,
        limits.maximum_identity_values_per_node,
    )?;
    for (kind, value) in node
        .identities
        .item
        .iter()
        .map(|value| ("item", value.to_string()))
        .chain(
            node.identities
                .representations
                .iter()
                .map(|value| ("representation", value.to_string())),
        )
        .chain(
            node.identities
                .tracks
                .iter()
                .map(|value| ("track", value.to_string())),
        )
        .chain(
            node.identities
                .assets
                .iter()
                .map(|value| ("asset", value.to_string())),
        )
        .chain(
            node.identities
                .derived_outputs
                .iter()
                .map(|value| ("derived_output", value.to_string())),
        )
    {
        if !entity_ids.insert(value.clone()) {
            return Err(GraphError::DuplicateEntityId { kind, id: value });
        }
    }
    for (field, value) in [("title", &node.title), ("description", &node.description)] {
        match value {
            TriState::Present(value) if value.len() > limits.maximum_string_bytes => {
                return Err(GraphError::StringTooLong {
                    field,
                    node: node.id.clone(),
                    actual: value.len(),
                    maximum: limits.maximum_string_bytes,
                });
            }
            _ => {}
        }
    }
    node.extensions
        .validate(limits.extensions)
        .map_err(|_| GraphError::InvalidExtensions {
            owner: node.id.to_string(),
        })
}

fn validate_redirects(
    graph: &SourceGraph,
    nodes: &BTreeMap<NodeId, &SourceNode>,
    maximum: usize,
) -> Result<(), GraphError> {
    let mut redirect_targets = BTreeMap::<&NodeId, Vec<&NodeId>>::new();
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::TransparentlyOverlays)
    {
        let Some(source) = nodes.get(&edge.from) else {
            return Err(GraphError::DanglingEdge {
                edge: edge.id.clone(),
                node: edge.from.clone(),
            });
        };
        if source.kind != NodeKind::Redirect {
            return Err(GraphError::InvalidRedirectSource {
                edge: edge.id.clone(),
                node: edge.from.clone(),
            });
        }
        redirect_targets
            .entry(&edge.from)
            .or_default()
            .push(&edge.to);
    }
    for (node, targets) in &redirect_targets {
        if targets.len() > 1 {
            return Err(GraphError::AmbiguousRedirect {
                node: (*node).clone(),
                targets: targets.len(),
            });
        }
    }
    for node in nodes
        .values()
        .filter(|node| node.kind == NodeKind::Redirect)
    {
        let mut seen = BTreeSet::new();
        let mut current = &node.id;
        for depth in 0..=maximum {
            if !seen.insert(current.clone()) {
                return Err(GraphError::RedirectCycle {
                    node: current.clone(),
                });
            }
            let Some(next) = redirect_targets
                .get(current)
                .and_then(|targets| targets.first())
            else {
                break;
            };
            if depth == maximum {
                return Err(GraphError::RedirectBudgetExceeded {
                    node: node.id.clone(),
                    maximum,
                });
            }
            current = next;
        }
    }
    Ok(())
}

fn validate_provenance_chains(
    nodes: &BTreeMap<NodeId, &SourceNode>,
    maximum: usize,
) -> Result<(), GraphError> {
    for &node in nodes.values() {
        let mut seen = BTreeSet::new();
        let mut current = node;
        for depth in 0..=maximum {
            if !seen.insert(current.id.clone()) {
                return Err(GraphError::ProvenanceCycle {
                    node: current.id.clone(),
                });
            }
            let Some(parent) = &current.provenance.parent_node else {
                break;
            };
            if depth == maximum {
                return Err(GraphError::ProvenanceBudgetExceeded {
                    node: node.id.clone(),
                    maximum,
                });
            }
            let Some(parent_node) = nodes.get(parent).copied() else {
                return Err(GraphError::DanglingProvenanceParent {
                    node: current.id.clone(),
                    parent: parent.clone(),
                });
            };
            current = parent_node;
        }
    }
    Ok(())
}

/// Serializable acquisition source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AcquisitionSource {
    DirectUrl {
        url: String,
    },
    Manifest {
        url: String,
        manifest_identity: String,
    },
    Fragments {
        manifest_identity: String,
        fragments: Vec<FragmentDescriptor>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentDescriptor {
    pub sequence: u64,
    pub url: String,
    pub expected_bytes: Option<u64>,
    pub checksum: Option<String>,
}

/// Data-only destination descriptor. Paths and addresses are strings and require adapter validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutputSinkSpec {
    AtomicFile {
        rooted_path: String,
    },
    Stdout,
    NamedPipe {
        rooted_path: String,
    },
    LocalHttp {
        bind: String,
        secret_reference: Option<String>,
    },
    Player {
        executable_reference: String,
        transport: PlayerTransport,
    },
    HostCallback {
        endpoint_id: String,
    },
    Null,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerTransport {
    Stdin,
    NamedPipe,
    LocalHttp,
    UrlPassthrough,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SinkSemantics {
    pub backpressure: BackpressureMode,
    pub seekable: bool,
    pub atomic: bool,
    pub postprocessing_requires_seekable_temporary: bool,
    pub expected_lifetime_seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BackpressureMode {
    BlockProducer,
    BoundedBuffer {
        maximum_items: u64,
        maximum_bytes: u64,
    },
    DropDeclaredTelemetryOnly,
}

impl AcquisitionSource {
    /// Validates count, text, sequence, and checksum bounds before acquisition planning.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a descriptor is empty, duplicated, malformed, or over limit.
    pub fn validate(&self, limits: DataContractLimits) -> Result<(), DataContractError> {
        match self {
            Self::DirectUrl { url } => validate_data_text("direct_url", url, limits),
            Self::Manifest {
                url,
                manifest_identity,
            } => {
                validate_data_text("manifest_url", url, limits)?;
                validate_data_text("manifest_identity", manifest_identity, limits)
            }
            Self::Fragments {
                manifest_identity,
                fragments,
            } => {
                validate_data_text("manifest_identity", manifest_identity, limits)?;
                if fragments.is_empty() {
                    return Err(DataContractError::EmptyCollection { field: "fragments" });
                }
                if fragments.len() > limits.maximum_fragments {
                    return Err(DataContractError::LimitExceeded {
                        field: "fragments",
                        actual: fragments.len() as u64,
                        maximum: limits.maximum_fragments as u64,
                    });
                }
                let mut sequences = BTreeSet::new();
                for fragment in fragments {
                    if !sequences.insert(fragment.sequence) {
                        return Err(DataContractError::DuplicateFragmentSequence {
                            sequence: fragment.sequence,
                        });
                    }
                    fragment.validate(limits)?;
                }
                Ok(())
            }
        }
    }
}

impl FragmentDescriptor {
    /// Validates one fragment's bounded text and optional checksum.
    ///
    /// # Errors
    ///
    /// Returns a typed error for empty/oversized URLs or malformed checksums.
    pub fn validate(&self, limits: DataContractLimits) -> Result<(), DataContractError> {
        validate_data_text("fragment_url", &self.url, limits)?;
        match &self.checksum {
            Some(checksum)
                if checksum.len() != 64
                    || !checksum
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
            {
                return Err(DataContractError::InvalidChecksum {
                    sequence: self.sequence,
                });
            }
            _ => {}
        }
        Ok(())
    }
}

impl OutputSinkSpec {
    /// Validates bounded sink text and capability-relative rooted path references.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an empty, oversized, absolute, or escaping reference.
    pub fn validate(&self, limits: DataContractLimits) -> Result<(), DataContractError> {
        match self {
            Self::AtomicFile { rooted_path } | Self::NamedPipe { rooted_path } => {
                validate_rooted_path_reference("rooted_path", rooted_path, limits)
            }
            Self::LocalHttp {
                bind,
                secret_reference,
            } => {
                validate_data_text("local_http_bind", bind, limits)?;
                if let Some(reference) = secret_reference {
                    validate_data_text("secret_reference", reference, limits)?;
                }
                Ok(())
            }
            Self::Player {
                executable_reference,
                ..
            } => validate_data_text("executable_reference", executable_reference, limits),
            Self::HostCallback { endpoint_id } => {
                validate_data_text("host_callback_endpoint", endpoint_id, limits)
            }
            Self::Stdout | Self::Null => Ok(()),
        }
    }
}

impl SinkSemantics {
    /// Validates lifetime and bounded-buffer capacity declarations.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a declared bound is zero or exceeds policy.
    pub fn validate(&self, limits: DataContractLimits) -> Result<(), DataContractError> {
        match self.expected_lifetime_seconds {
            Some(lifetime) if lifetime == 0 || lifetime > limits.maximum_sink_lifetime_seconds => {
                return Err(DataContractError::LimitExceeded {
                    field: "expected_lifetime_seconds",
                    actual: lifetime,
                    maximum: limits.maximum_sink_lifetime_seconds,
                });
            }
            _ => {}
        }
        match self.backpressure {
            BackpressureMode::BoundedBuffer {
                maximum_items,
                maximum_bytes,
            } if maximum_items == 0
                || maximum_bytes == 0
                || maximum_items > limits.maximum_buffer_items
                || maximum_bytes > limits.maximum_buffer_bytes =>
            {
                return Err(DataContractError::InvalidBoundedBuffer);
            }
            _ => {}
        }
        Ok(())
    }
}

fn validate_data_text(
    field: &'static str,
    value: &str,
    limits: DataContractLimits,
) -> Result<(), DataContractError> {
    if value.is_empty() {
        return Err(DataContractError::EmptyText { field });
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(DataContractError::InvalidText { field });
    }
    if value.len() > limits.maximum_text_bytes {
        return Err(DataContractError::LimitExceeded {
            field,
            actual: value.len() as u64,
            maximum: limits.maximum_text_bytes as u64,
        });
    }
    Ok(())
}

fn validate_rooted_path_reference(
    field: &'static str,
    value: &str,
    limits: DataContractLimits,
) -> Result<(), DataContractError> {
    if value.len() > limits.maximum_path_reference_bytes {
        return Err(DataContractError::LimitExceeded {
            field,
            actual: value.len() as u64,
            maximum: limits.maximum_path_reference_bytes as u64,
        });
    }
    if value.is_empty()
        || Path::new(value).is_absolute()
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(DataContractError::InvalidPathReference {
            field,
            value: value.to_owned(),
        });
    }
    let normalized = value.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized.contains(':')
        || normalized.split('/').any(|segment| {
            segment.is_empty() || matches!(segment, "." | "..") || segment.ends_with(['.', ' '])
        })
    {
        return Err(DataContractError::InvalidPathReference {
            field,
            value: value.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> Result<SourceNode, crate::IdError> {
        Ok(SourceNode {
            id: NodeId::new(format!("node_{name}"))?,
            kind: NodeKind::Media,
            identities: EntityIdentities::default(),
            provenance: Provenance {
                extractor_key: "fixture".into(),
                source_identity: name.into(),
                canonical_url: format!("https://example.test/{name}"),
                parent_node: None,
            },
            title: TriState::Unknown,
            description: TriState::NotApplicable,
            extensions: ExtensionMap::default(),
        })
    }

    #[test]
    fn rejects_dangling_root() -> Result<(), crate::IdError> {
        let graph = SourceGraph {
            schema: SchemaVersion { major: 1, minor: 0 },
            roots: vec![NodeId::new("node_missing")?],
            nodes: vec![],
            edges: vec![],
            continuations: vec![],
        };
        assert!(matches!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::DanglingRoot { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_dangling_provenance_parent() -> Result<(), crate::IdError> {
        let mut child = node("child")?;
        child.provenance.parent_node = Some(NodeId::new("node_missing_parent")?);
        let graph = SourceGraph {
            schema: SchemaVersion { major: 1, minor: 0 },
            roots: vec![child.id.clone()],
            nodes: vec![child],
            edges: vec![],
            continuations: vec![],
        };
        let decoded = serde_json::to_vec(&graph)
            .ok()
            .and_then(|wire| serde_json::from_slice::<SourceGraph>(&wire).ok());
        assert!(matches!(
            decoded.as_ref().map(|graph| graph.validate(GraphLimits::default())),
            Some(Err(GraphError::DanglingProvenanceParent { node, parent }))
                if node.as_str() == "node_child"
                    && parent.as_str() == "node_missing_parent"
        ));
        Ok(())
    }

    #[test]
    fn rejects_redirect_with_multiple_overlay_targets() -> Result<(), crate::IdError> {
        let mut redirect = node("redirect")?;
        redirect.kind = NodeKind::Redirect;
        let first = node("first")?;
        let second = node("second")?;
        let first_edge = SourceEdge {
            id: EdgeId::new("edge_redirect_first")?,
            from: redirect.id.clone(),
            to: first.id.clone(),
            kind: EdgeKind::TransparentlyOverlays,
        };
        let second_edge = SourceEdge {
            id: EdgeId::new("edge_redirect_second")?,
            from: redirect.id.clone(),
            to: second.id.clone(),
            kind: EdgeKind::TransparentlyOverlays,
        };
        for edges in [
            vec![first_edge.clone(), second_edge.clone()],
            vec![second_edge, first_edge],
        ] {
            let graph = SourceGraph {
                schema: SchemaVersion { major: 1, minor: 0 },
                roots: vec![redirect.id.clone()],
                nodes: vec![redirect.clone(), first.clone(), second.clone()],
                edges,
                continuations: vec![],
            };
            assert_eq!(
                graph.validate(GraphLimits::default()),
                Err(GraphError::AmbiguousRedirect {
                    node: redirect.id.clone(),
                    targets: 2,
                })
            );
        }
        Ok(())
    }

    #[test]
    fn overlay_edges_require_redirect_sources_and_cycles_always_fail() -> Result<(), crate::IdError>
    {
        let first = node("overlay_first")?;
        let second = node("overlay_second")?;
        let first_edge = SourceEdge {
            id: EdgeId::new("edge_overlay_first")?,
            from: first.id.clone(),
            to: second.id.clone(),
            kind: EdgeKind::TransparentlyOverlays,
        };
        let second_edge = SourceEdge {
            id: EdgeId::new("edge_overlay_second")?,
            from: second.id.clone(),
            to: first.id.clone(),
            kind: EdgeKind::TransparentlyOverlays,
        };
        let mut graph = SourceGraph {
            schema: SchemaVersion { major: 1, minor: 0 },
            roots: vec![first.id.clone()],
            nodes: vec![first.clone(), second.clone()],
            edges: vec![first_edge.clone(), second_edge],
            continuations: vec![],
        };
        assert_eq!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::InvalidRedirectSource {
                edge: first_edge.id,
                node: first.id.clone(),
            })
        );
        graph.nodes[0].kind = NodeKind::Redirect;
        graph.nodes[1].kind = NodeKind::Redirect;
        assert_eq!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::RedirectCycle { node: first.id })
        );
        Ok(())
    }

    #[test]
    fn every_traversable_relationship_rejects_cycles_and_accepts_exact_limit_acyclic_graphs()
    -> Result<(), crate::IdError> {
        for kind in [
            EdgeKind::Contains,
            EdgeKind::Embeds,
            EdgeKind::TransparentlyOverlays,
            EdgeKind::Continuation,
        ] {
            let mut first = node("traversal_first")?;
            let mut second = node("traversal_second")?;
            let mut third = node("traversal_third")?;
            if kind == EdgeKind::TransparentlyOverlays {
                first.kind = NodeKind::Redirect;
                second.kind = NodeKind::Redirect;
                third.kind = NodeKind::Redirect;
            }

            let expected_cycle = |node: NodeId| {
                if kind == EdgeKind::TransparentlyOverlays {
                    GraphError::RedirectCycle { node }
                } else {
                    GraphError::RelationshipCycle { node }
                }
            };
            let limits = GraphLimits {
                maximum_nodes: 3,
                maximum_edges: 2,
                ..GraphLimits::default()
            };

            let self_cycle = SourceGraph {
                schema: SchemaVersion { major: 1, minor: 0 },
                roots: vec![first.id.clone()],
                nodes: vec![first.clone()],
                edges: vec![SourceEdge {
                    id: EdgeId::new("edge_traversal_self")?,
                    from: first.id.clone(),
                    to: first.id.clone(),
                    kind: kind.clone(),
                }],
                continuations: vec![],
            };
            assert_eq!(
                self_cycle.validate(limits),
                Err(expected_cycle(first.id.clone()))
            );

            let multi_node_cycle = SourceGraph {
                schema: SchemaVersion { major: 1, minor: 0 },
                roots: vec![first.id.clone()],
                nodes: vec![first.clone(), second.clone()],
                edges: vec![
                    SourceEdge {
                        id: EdgeId::new("edge_traversal_cycle_first")?,
                        from: first.id.clone(),
                        to: second.id.clone(),
                        kind: kind.clone(),
                    },
                    SourceEdge {
                        id: EdgeId::new("edge_traversal_cycle_second")?,
                        from: second.id.clone(),
                        to: first.id.clone(),
                        kind: kind.clone(),
                    },
                ],
                continuations: vec![],
            };
            assert_eq!(
                multi_node_cycle.validate(limits),
                Err(expected_cycle(first.id.clone()))
            );

            let acyclic_at_limit = SourceGraph {
                schema: SchemaVersion { major: 1, minor: 0 },
                roots: vec![first.id.clone()],
                nodes: vec![first.clone(), second.clone(), third.clone()],
                edges: vec![
                    SourceEdge {
                        id: EdgeId::new("edge_traversal_acyclic_first")?,
                        from: first.id.clone(),
                        to: second.id.clone(),
                        kind: kind.clone(),
                    },
                    SourceEdge {
                        id: EdgeId::new("edge_traversal_acyclic_second")?,
                        from: second.id.clone(),
                        to: third.id.clone(),
                        kind,
                    },
                ],
                continuations: vec![],
            };
            assert!(acyclic_at_limit.validate(limits).is_ok());
        }
        Ok(())
    }

    #[test]
    fn deep_bounded_acyclic_traversal_graph_is_accepted() -> Result<(), crate::IdError> {
        const NODE_COUNT: usize = 256;
        let nodes = (0..NODE_COUNT)
            .map(|index| node(&format!("deep_{index}")))
            .collect::<Result<Vec<_>, _>>()?;
        let edges = nodes
            .windows(2)
            .enumerate()
            .map(|(index, pair)| {
                Ok(SourceEdge {
                    id: EdgeId::new(format!("edge_deep_{index}"))?,
                    from: pair[0].id.clone(),
                    to: pair[1].id.clone(),
                    kind: EdgeKind::Contains,
                })
            })
            .collect::<Result<Vec<_>, crate::IdError>>()?;
        let graph = SourceGraph {
            schema: SchemaVersion { major: 1, minor: 0 },
            roots: vec![nodes[0].id.clone()],
            nodes,
            edges,
            continuations: vec![],
        };
        assert!(
            graph
                .validate(GraphLimits {
                    maximum_nodes: NODE_COUNT,
                    maximum_edges: NODE_COUNT - 1,
                    ..GraphLimits::default()
                })
                .is_ok()
        );
        Ok(())
    }

    #[test]
    fn non_traversable_relationship_cycles_remain_descriptive() -> Result<(), crate::IdError> {
        for kind in [
            EdgeKind::Alternate,
            EdgeKind::Complementary,
            EdgeKind::Additional,
            EdgeKind::DerivedOutput,
        ] {
            let first = node("descriptive_first")?;
            let second = node("descriptive_second")?;
            let graph = SourceGraph {
                schema: SchemaVersion { major: 1, minor: 0 },
                roots: vec![first.id.clone()],
                nodes: vec![first.clone(), second.clone()],
                edges: vec![
                    SourceEdge {
                        id: EdgeId::new("edge_descriptive_first")?,
                        from: first.id.clone(),
                        to: second.id.clone(),
                        kind: kind.clone(),
                    },
                    SourceEdge {
                        id: EdgeId::new("edge_descriptive_second")?,
                        from: second.id.clone(),
                        to: first.id,
                        kind,
                    },
                ],
                continuations: vec![],
            };
            assert!(graph.validate(GraphLimits::default()).is_ok());
        }
        Ok(())
    }

    #[test]
    fn round_trip_preserves_tri_state() -> Result<(), crate::IdError> {
        let source = node("one")?;
        let encoded = serde_json::to_vec(&source);
        assert!(encoded.is_ok());
        let decoded = encoded
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SourceNode>(&bytes).ok());
        assert_eq!(decoded, Some(source));
        Ok(())
    }

    #[test]
    fn acquisition_rejects_duplicate_fragments_and_bad_checksum() {
        let fragment = FragmentDescriptor {
            sequence: 7,
            url: "https://example.test/fragment".into(),
            expected_bytes: Some(10),
            checksum: Some("x".repeat(64)),
        };
        assert_eq!(
            fragment.validate(DataContractLimits::default()),
            Err(DataContractError::InvalidChecksum { sequence: 7 })
        );
        let source = AcquisitionSource::Fragments {
            manifest_identity: "manifest-one".into(),
            fragments: vec![
                FragmentDescriptor {
                    checksum: None,
                    ..fragment.clone()
                },
                FragmentDescriptor {
                    checksum: None,
                    ..fragment
                },
            ],
        };
        assert_eq!(
            source.validate(DataContractLimits::default()),
            Err(DataContractError::DuplicateFragmentSequence { sequence: 7 })
        );
    }

    #[test]
    fn sink_rejects_escaping_path_and_unbounded_capacity() {
        let sink = OutputSinkSpec::AtomicFile {
            rooted_path: "../escape.bin".into(),
        };
        assert!(matches!(
            sink.validate(DataContractLimits::default()),
            Err(DataContractError::InvalidPathReference { .. })
        ));
        let semantics = SinkSemantics {
            backpressure: BackpressureMode::BoundedBuffer {
                maximum_items: 0,
                maximum_bytes: 1024,
            },
            seekable: false,
            atomic: false,
            postprocessing_requires_seekable_temporary: false,
            expected_lifetime_seconds: Some(1),
        };
        assert_eq!(
            semantics.validate(DataContractLimits::default()),
            Err(DataContractError::InvalidBoundedBuffer)
        );
    }

    #[test]
    fn graph_rejects_schema_duplicate_roots_and_provenance_cycles() -> Result<(), crate::IdError> {
        let mut source = node("cycle")?;
        source.provenance.parent_node = Some(source.id.clone());
        let mut graph = SourceGraph {
            schema: SchemaVersion { major: 0, minor: 0 },
            roots: vec![source.id.clone()],
            nodes: vec![source.clone()],
            edges: vec![],
            continuations: vec![],
        };
        assert!(matches!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::InvalidSchema { .. })
        ));
        graph.schema = SchemaVersion { major: 1, minor: 0 };
        assert_eq!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::ProvenanceCycle {
                node: source.id.clone()
            })
        );
        graph.nodes[0].provenance.parent_node = None;
        graph.roots.push(source.id.clone());
        assert_eq!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::DuplicateId {
                kind: "root",
                id: source.id.to_string()
            })
        );
        graph.roots.pop();
        graph.nodes[0].provenance.extractor_key.clear();
        assert_eq!(
            graph.validate(GraphLimits::default()),
            Err(GraphError::InvalidString {
                field: "extractor_key",
                node: source.id
            })
        );
        Ok(())
    }

    #[test]
    fn graph_bounds_identity_collections_without_clone_amplification() -> Result<(), crate::IdError>
    {
        let mut source = node("identities")?;
        source.identities.representations = vec![RepresentationId::new("repr_one")?];
        source.identities.tracks = vec![TrackId::new("track_one")?];
        source.identities.assets = vec![AssetId::new("asset_one")?];
        let graph = SourceGraph {
            schema: SchemaVersion { major: 1, minor: 0 },
            roots: vec![source.id.clone()],
            nodes: vec![source],
            edges: vec![],
            continuations: vec![],
        };
        assert_eq!(
            graph.validate(GraphLimits {
                maximum_identity_values_per_node: 2,
                ..GraphLimits::default()
            }),
            Err(GraphError::LimitExceeded {
                field: "identities_per_node",
                actual: 3,
                maximum: 2,
            })
        );
        Ok(())
    }

    #[test]
    fn public_tagged_enums_reject_unknown_fields() {
        assert!(
            serde_json::from_str::<AcquisitionSource>(
                r#"{"kind":"direct_url","url":"https://example.test/a","smuggled":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<OutputSinkSpec>(
                r#"{"kind":"atomic_file","rooted_path":"safe.bin","smuggled":true}"#
            )
            .is_err()
        );
    }
}
