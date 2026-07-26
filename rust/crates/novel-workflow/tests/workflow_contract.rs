use agent_runtime::OutputContract;
use knowledge_core::{ContextBlock, ContextSnapshot};
use novel_workflow::{
    canon_extractor_profile, reviewer_profile, writer_profile, NovelWorkflowBudget,
    NovelWorkflowDefinition, NovelWorkflowModels, ProfileModel,
};
use task_engine::NodeKind;

fn snapshot() -> ContextSnapshot {
    ContextSnapshot::new(
        "context-novel-task-1",
        vec![ContextBlock::new("task-environment", "frozen novel input").unwrap()],
    )
    .unwrap()
}

fn models() -> NovelWorkflowModels {
    NovelWorkflowModels {
        writer: ProfileModel::new("xiaomi", "mimo-v2.5-pro"),
        reviewer: ProfileModel::new("xiaomi", "mimo-v2.5-pro"),
        canon_extractor: ProfileModel::new("xiaomi", "mimo-v2.5-pro"),
    }
}

fn budget() -> NovelWorkflowBudget {
    NovelWorkflowBudget {
        input_tokens: 12_000,
        output_tokens: 8_000,
    }
}

#[test]
fn profiles_are_explicit_json_contracts_without_knowledge_write_tools() {
    let profiles = [
        writer_profile().unwrap(),
        reviewer_profile().unwrap(),
        canon_extractor_profile().unwrap(),
    ];
    for profile in profiles {
        assert!(matches!(
            profile.output_contract,
            OutputContract::Json { .. }
        ));
        assert!(profile.tool_grant.iter().next().is_none());
    }
}

#[test]
fn default_chapter_is_writer_reviewer_extractor_dag_with_frozen_context() {
    let frozen = snapshot();
    let definition = NovelWorkflowDefinition::default_chapter(
        "task-1",
        "Write chapter one",
        frozen.clone(),
        models(),
        budget(),
    )
    .unwrap();
    let task = definition.task_run();
    assert_eq!(task.workflow, "novel.default-chapter");
    assert_eq!(task.origin_kind, "novel_task");
    assert_eq!(task.config_version, "novel-workflow-v1");
    assert_eq!(task.nodes.len(), 3);
    assert_eq!(
        task.resolved_config["context_content_hash"],
        frozen.content_hash
    );
    assert_eq!(
        task.resolved_config["context_snapshot"]["context_snapshot_id"],
        frozen.context_snapshot_id
    );

    let writer = &task.nodes[0];
    let reviewer = &task.nodes[1];
    let extractor = &task.nodes[2];
    assert_eq!(writer.kind, NodeKind::Model);
    assert!(writer.dependencies.is_empty());
    assert_eq!(reviewer.kind, NodeKind::Reviewer);
    assert_eq!(reviewer.dependencies, vec![writer.node_id.clone()]);
    assert_eq!(extractor.dependencies, vec![reviewer.node_id.clone()]);
}

#[test]
fn competitive_outline_fans_out_blind_reviews_per_candidate() {
    let definition = NovelWorkflowDefinition::competitive_outline(
        "outline-1",
        "Propose a volume outline",
        snapshot(),
        models(),
        budget(),
        3,
        2,
    )
    .unwrap();
    let task = definition.task_run();
    let writers = task
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Model)
        .collect::<Vec<_>>();
    let reviewers = task
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Reviewer)
        .collect::<Vec<_>>();
    assert_eq!(writers.len(), 3);
    assert_eq!(reviewers.len(), 6);
    assert!(reviewers.iter().all(|node| node.dependencies.len() == 1));
    assert!(reviewers.iter().all(|reviewer| writers
        .iter()
        .any(|writer| reviewer.dependencies == vec![writer.node_id.clone()])));
}
