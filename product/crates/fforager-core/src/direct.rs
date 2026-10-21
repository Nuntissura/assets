//! Pure direct-media graph construction and deterministic single-representation selection.

use fforager_contracts::{
    AcquisitionSource, AssetId, DataContractError, DataContractLimits, EntityIdentities,
    ExtensionMap, GraphError, GraphLimits, IdError, ItemId, NodeId, NodeKind, Provenance,
    RepresentationId, SchemaVersion, SourceGraph, SourceNode, TriState,
};
use std::fmt;

/// A selected representation and asset remain attached to their canonical graph source.
#[derive(Clone, Debug)]
pub struct DirectPlan {
    pub graph: SourceGraph,
    pub acquisition: AcquisitionSource,
    pub selected_representation: RepresentationId,
    pub selected_asset: AssetId,
}

#[derive(Debug)]
pub enum DirectPlanError {
    InvalidIdentity,
    InvalidUrl,
    Identity(IdError),
    Graph(GraphError),
    Acquisition(DataContractError),
    Selection,
}

impl fmt::Display for DirectPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let detail = match self {
            Self::InvalidIdentity => "source identity must be 64 lowercase hexadecimal characters",
            Self::InvalidUrl => {
                "direct acquisition requires a bounded HTTP(S) URL without credentials or fragments"
            }
            Self::Identity(_) => "derived identity violates the canonical identifier contract",
            Self::Graph(_) => "direct source graph violates its canonical contract",
            Self::Acquisition(_) => "direct acquisition violates its canonical contract",
            Self::Selection => {
                "selected representation and asset do not belong to the single source"
            }
        };
        write!(formatter, "FF-DIRECT-E-PLAN: {detail}")
    }
}

impl std::error::Error for DirectPlanError {}

/// Constructs and selects the sole direct-media representation using an actual source digest.
///
/// Networking separately parses the URL and admits its resolved destination before execution.
/// # Errors
/// Rejects malformed identities, invalid direct descriptors, or canonical graph/selection violations.
pub fn plan_direct(url: &str, sha256identity: &str) -> Result<DirectPlan, DirectPlanError> {
    validate_source_identity(sha256identity)?;
    validate_direct_url(url)?;
    let node = NodeId::new(format!("node_{sha256identity}")).map_err(DirectPlanError::Identity)?;
    let item = ItemId::new(format!("item_{sha256identity}")).map_err(DirectPlanError::Identity)?;
    let representation = RepresentationId::new(format!("repr_{sha256identity}"))
        .map_err(DirectPlanError::Identity)?;
    let asset =
        AssetId::new(format!("asset_{sha256identity}")).map_err(DirectPlanError::Identity)?;
    let graph = SourceGraph {
        schema: SchemaVersion { major: 1, minor: 0 },
        roots: vec![node.clone()],
        nodes: vec![SourceNode {
            id: node,
            kind: NodeKind::Media,
            identities: EntityIdentities {
                item: Some(item),
                representations: vec![representation.clone()],
                assets: vec![asset.clone()],
                ..EntityIdentities::default()
            },
            provenance: Provenance {
                extractor_key: "direct_http".to_owned(),
                source_identity: sha256identity.to_owned(),
                canonical_url: url.to_owned(),
                parent_node: None,
            },
            title: TriState::Unknown,
            description: TriState::Unknown,
            extensions: ExtensionMap::default(),
        }],
        edges: Vec::new(),
        continuations: Vec::new(),
    };
    graph
        .validate(direct_graph_limits())
        .map_err(DirectPlanError::Graph)?;
    let acquisition = AcquisitionSource::DirectUrl {
        url: url.to_owned(),
    };
    acquisition
        .validate(DataContractLimits::default())
        .map_err(DirectPlanError::Acquisition)?;
    let plan = DirectPlan {
        graph,
        acquisition,
        selected_representation: representation,
        selected_asset: asset,
    };
    plan.validate()?;
    Ok(plan)
}

impl DirectPlan {
    /// Revalidates graph, acquisition, and the selected source relationship before execution.
    ///
    /// # Errors
    /// Rejects an altered graph, acquisition URL, or selected entity.
    pub fn validate(&self) -> Result<(), DirectPlanError> {
        self.graph
            .validate(direct_graph_limits())
            .map_err(DirectPlanError::Graph)?;
        self.acquisition
            .validate(DataContractLimits::default())
            .map_err(DirectPlanError::Acquisition)?;
        let [node] = self.graph.nodes.as_slice() else {
            return Err(DirectPlanError::Selection);
        };
        let AcquisitionSource::DirectUrl { url } = &self.acquisition else {
            return Err(DirectPlanError::Selection);
        };
        validate_direct_url(url)?;
        let identity = &node.provenance.source_identity;
        validate_source_identity(identity)?;
        let expected_item = format!("item_{identity}");
        if node.kind != NodeKind::Media
            || self.graph.roots.as_slice() != [node.id.clone()]
            || node.id.as_str() != format!("node_{identity}")
            || node.identities.item.as_ref().map(ItemId::as_str) != Some(expected_item.as_str())
            || self.selected_representation.as_str() != format!("repr_{identity}")
            || self.selected_asset.as_str() != format!("asset_{identity}")
            || node.identities.representations.as_slice() != [self.selected_representation.clone()]
            || node.identities.assets.as_slice() != [self.selected_asset.clone()]
            || !node.identities.tracks.is_empty()
            || !node.identities.derived_outputs.is_empty()
            || node.provenance.canonical_url != *url
        {
            return Err(DirectPlanError::Selection);
        }
        Ok(())
    }
}

fn direct_graph_limits() -> GraphLimits {
    GraphLimits {
        maximum_roots: 1,
        maximum_nodes: 1,
        maximum_edges: 0,
        maximum_continuations: 0,
        maximum_identity_values_per_node: 3,
        maximum_string_bytes: 8 * 1024,
        ..GraphLimits::default()
    }
}

fn validate_direct_url(url: &str) -> Result<(), DirectPlanError> {
    let authority = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .and_then(|rest| rest.split(['/', '?', '#']).next());
    if url.len() > 8 * 1024
        || url
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || url.contains(['#', '\\'])
        || authority.is_none_or(|authority| authority.is_empty() || authority.contains('@'))
    {
        return Err(DirectPlanError::InvalidUrl);
    }
    Ok(())
}

fn validate_source_identity(identity: &str) -> Result<(), DirectPlanError> {
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DirectPlanError::InvalidIdentity);
    }
    Ok(())
}
