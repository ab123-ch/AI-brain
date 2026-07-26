use std::collections::HashSet;
use std::fmt::Write as _;

use crate::{CanonStatus, NovelFact, NovelFactKind, NovelProject, NovelRecallPack, NovelTaskType};

#[must_use]
pub fn build_recall_pack(project: &NovelProject, task_type: NovelTaskType) -> NovelRecallPack {
    let included = included_kinds(&task_type);
    let chapter = project.current_chapter;
    let mut facts = project
        .facts
        .iter()
        .filter(|fact| fact.branch_id == project.active_branch)
        .filter(|fact| fact.status == CanonStatus::Confirmed)
        .filter(|fact| included.contains(&fact.kind))
        .filter(|fact| is_active_at(fact, chapter))
        .cloned()
        .collect::<Vec<_>>();
    facts.sort_by(|left, right| {
        right
            .valid_from_chapter
            .cmp(&left.valid_from_chapter)
            .then_with(|| right.revision.cmp(&left.revision))
    });
    facts.truncate(80);
    let rendered_context = render(project, &task_type, &facts);
    NovelRecallPack {
        project_id: project.project_id.clone(),
        project_title: project.title.clone(),
        revision: project.canon_revision,
        task_type,
        branch_id: project.active_branch.clone(),
        current_chapter: project.current_chapter,
        rendered_context,
        facts,
    }
}

fn included_kinds(task_type: &NovelTaskType) -> HashSet<NovelFactKind> {
    use NovelFactKind as K;
    let kinds: &[K] = match task_type {
        NovelTaskType::Outline | NovelTaskType::VolumeOutline => &[
            K::WorldRule,
            K::Character,
            K::PlotThread,
            K::Outline,
            K::Decision,
            K::Feedback,
            K::WritingExperience,
        ],
        NovelTaskType::ChapterPlan => &[
            K::WorldRule,
            K::Character,
            K::CharacterState,
            K::Event,
            K::Timeline,
            K::PlotThread,
            K::Foreshadowing,
            K::Outline,
            K::ChapterSummary,
            K::Decision,
            K::WritingExperience,
        ],
        NovelTaskType::Body | NovelTaskType::Continuation | NovelTaskType::Polish => &[
            K::WorldRule,
            K::Character,
            K::CharacterState,
            K::Location,
            K::Organization,
            K::Item,
            K::Event,
            K::Timeline,
            K::PlotThread,
            K::Foreshadowing,
            K::ChapterPlan,
            K::ChapterSummary,
            K::Decision,
            K::WritingExperience,
        ],
        NovelTaskType::Review | NovelTaskType::Retrospective => &[
            K::WorldRule,
            K::Character,
            K::CharacterState,
            K::Event,
            K::Timeline,
            K::PlotThread,
            K::Foreshadowing,
            K::Outline,
            K::ChapterPlan,
            K::ChapterSummary,
            K::Decision,
            K::Feedback,
            K::WritingExperience,
        ],
    };
    kinds.iter().cloned().collect()
}

fn is_active_at(fact: &NovelFact, chapter: Option<u32>) -> bool {
    let Some(chapter) = chapter else {
        return true;
    };
    fact.valid_from_chapter.is_none_or(|start| start <= chapter)
        && fact.valid_to_chapter.is_none_or(|end| chapter <= end)
}

fn render(project: &NovelProject, task_type: &NovelTaskType, facts: &[NovelFact]) -> String {
    let mut output = format!(
        "[Novel project memory]\nProject: {} ({})\nBranch: {}\nCanon revision: {}\nTask: {:?}\n",
        project.title, project.project_id, project.active_branch, project.canon_revision, task_type
    );
    if let Some(platform) = &project.target_platform {
        let _ = writeln!(output, "Target platform: {platform}");
    }
    if let Some(chapter) = project.current_chapter {
        let _ = writeln!(output, "Current chapter: {chapter}");
    }
    output.push_str("\nActive facts:\n");
    for fact in facts {
        let _ = writeln!(
            output,
            "- [{:?}] {}: {} (id={}, rev={})",
            fact.kind, fact.title, fact.summary, fact.fact_id, fact.revision
        );
    }
    output
}
