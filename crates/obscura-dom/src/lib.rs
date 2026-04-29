#[macro_use]
extern crate html5ever;

pub mod selector;
pub mod serialize;
pub mod tree;
pub mod tree_sink;

pub use tree::{Attribute, DomTree, Node, NodeData, NodeId};
pub use tree_sink::{parse_fragment, parse_html};
