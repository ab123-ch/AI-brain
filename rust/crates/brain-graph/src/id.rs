//! ID 生成器（见设计 3.10）
//!
//! 格式：`{graph_type}_{kind}_{uuid8}`
//! 示例：memory_concept_a1b2c3d4, code_file_e5f6g7h8

use crate::schema::{GraphType, NodeKind};
use uuid::Uuid;

/// 生成节点 ID
///
/// 格式：`{graph_type}_{kind}_{uuid8}`
/// - graph_type: Memory/Code/Novel/Video 或 Custom(name)
/// - kind: Memory/Concept/Entity/Tool/Code
/// - uuid8: 取 UUID 前 8 位
///
/// # 示例
/// ```ignore
/// gen_node_id(GraphType::Memory, NodeKind::Concept) // "memory_concept_a1b2c3d4"
/// gen_node_id(GraphType::Code, NodeKind::Code)       // "code_code_e5f6g7h8"
/// gen_node_id(GraphType::Custom("game"), NodeKind::Entity) // "custom(game)_entity_i9j0k1l2"
/// ```
#[must_use]
pub fn gen_node_id(graph_type: GraphType, kind: NodeKind) -> String {
    // 构建 prefix: graph_type_kind
    let gt_str = match &graph_type {
        GraphType::Memory => "memory",
        GraphType::Code => "code",
        GraphType::Novel => "novel",
        GraphType::Video => "video",
        GraphType::Custom(name) => &format!("custom({name})"),
    };
    let kind_str = match kind {
        NodeKind::Memory => "memory",
        NodeKind::Concept => "concept",
        NodeKind::Entity => "entity",
        NodeKind::Tool => "tool",
        NodeKind::Code => "code",
    };
    let prefix = format!("{gt_str}_{kind_str}");

    // 生成 UUID 并取前 8 位
    let uuid = Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &uuid[..8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_has_correct_prefix() {
        let id = gen_node_id(GraphType::Memory, NodeKind::Concept);
        assert!(id.starts_with("memory_concept_"));
        assert_eq!(id.len(), "memory_concept_".len() + 8);
    }

    #[test]
    fn id_code_file_prefix() {
        let id = gen_node_id(GraphType::Code, NodeKind::Code);
        assert!(id.starts_with("code_code_"));
    }

    #[test]
    fn id_custom_domain() {
        let id = gen_node_id(GraphType::Custom("game".into()), NodeKind::Entity);
        assert!(id.starts_with("custom(game)_entity_"));
    }

    #[test]
    fn id_unique() {
        let a = gen_node_id(GraphType::Memory, NodeKind::Concept);
        let b = gen_node_id(GraphType::Memory, NodeKind::Concept);
        assert_ne!(a, b);
    }
}
