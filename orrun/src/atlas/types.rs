//! Atlas graph value types.

use super::features::{EndpointKind, Kind, NodeKind};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Port {
    pub id: i32,
    pub t: f32,
    pub kind: Kind,
    pub feature_class: i32,
    pub flow_sign: i32,
    /// Quantized metres at the crossing.
    pub surface_z: i32,
    pub feature_id: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Endpoint {
    pub kind: EndpointKind,
    /// EDGE_PORT: edge key. NODE: node id. LAKE: lake id.
    pub ref_id: i32,
    pub port_id: i32,
}

impl Endpoint {
    pub fn edge_port(edge_key: i32, port_id: i32) -> Self {
        Self {
            kind: EndpointKind::EdgePort,
            ref_id: edge_key,
            port_id,
        }
    }

    pub fn ocean() -> Self {
        Self {
            kind: EndpointKind::Ocean,
            ref_id: 0,
            port_id: 0,
        }
    }

    pub fn lake(lake_id: i32) -> Self {
        Self {
            kind: EndpointKind::Lake,
            ref_id: lake_id,
            port_id: 0,
        }
    }

    pub fn node(node_id: i32) -> Self {
        Self {
            kind: EndpointKind::Node,
            ref_id: node_id,
            port_id: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub a: Endpoint,
    pub b: Endpoint,
    pub kind: Kind,
    pub feature_class: i32,
    pub feature_id: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lake {
    pub id: i32,
    pub cells: Vec<i32>,
    pub spill_cell: i32,
    pub surface_code: i32,
    pub surface_z: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    pub id: i32,
    pub kind: NodeKind,
    pub cell: i32,
    pub ax: i32,
    pub az: i32,
    pub landmass: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossing {
    pub id: i32,
    pub cell: i32,
    pub river_id: i32,
    pub road_id: i32,
    pub river_class: i32,
    pub road_class: i32,
}
