use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use brain_graph::schema::{Edge, EdgeKind, GraphType, Node, NodeKind};
use brain_graph::store::GraphStore;
use chrono::Utc;
use serde_json::json;

use crate::error::{MemoryError, Result};

use super::recall::build_recall_pack;
use super::schema::{
    CanonStatus, CommitReport, ConflictRecord, NovelFact, NovelMemoryDelta, NovelProject,
    NovelRecallPack, NovelTaskType, ProposedFact,
};

pub(crate) struct NovelMemoryStore {
    root: PathBuf,
    graph_db_path: Option<PathBuf>,
}

impl NovelMemoryStore {
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>, graph_db_path: Option<PathBuf>) -> Self {
        Self {
            root: base_dir.into().join("novel").join("projects"),
            graph_db_path,
        }
    }

    pub fn create_project(&self, project: &NovelProject) -> Result<()> {
        validate_project_id(&project.project_id)?;
        if project.title.trim().is_empty() {
            return Err(MemoryError::Conflict("小说项目标题不能为空".into()));
        }
        let path = self.project_path(&project.project_id);
        if path.exists() {
            return Err(MemoryError::Conflict(format!(
                "小说项目已存在: {}",
                project.project_id
            )));
        }
        self.write_project(project)?;
        let _ = self.mirror_to_graph(project);
        Ok(())
    }

    pub fn load_project(&self, project_id: &str) -> Result<NovelProject> {
        validate_project_id(project_id)?;
        let path = self.project_path(project_id);
        if !path.exists() {
            return Err(MemoryError::NotFound(format!(
                "小说项目不存在: {project_id}"
            )));
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    pub fn list_projects(&self) -> Result<Vec<NovelProject>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut projects = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            projects.push(serde_json::from_str(&std::fs::read_to_string(path)?)?);
        }
        projects.sort_by(|left: &NovelProject, right| right.updated_at.cmp(&left.updated_at));
        Ok(projects)
    }

    pub fn recall(&self, project_id: &str, task_type: NovelTaskType) -> Result<NovelRecallPack> {
        Ok(build_recall_pack(
            &self.load_project(project_id)?,
            task_type,
        ))
    }

    /// 标记一条 Canon 冲突已由主脑或用户审查，不直接修改事实本身。
    pub fn resolve_conflict(
        &self,
        project_id: &str,
        conflict_id: &str,
        resolution: &str,
    ) -> Result<()> {
        if resolution.trim().is_empty() {
            return Err(MemoryError::Conflict("冲突处理说明不能为空".into()));
        }
        let mut project = self.load_project(project_id)?;
        let conflict = project
            .conflicts
            .iter_mut()
            .find(|item| item.conflict_id == conflict_id)
            .ok_or_else(|| MemoryError::NotFound(format!("冲突不存在: {conflict_id}")))?;
        conflict.resolved = true;
        conflict.resolution = Some(resolution.to_owned());
        project.updated_at = Utc::now().timestamp_millis();
        self.write_project(&project)
    }

    pub fn check_consistency(&self, project_id: &str) -> Result<super::ConsistencyReport> {
        Ok(super::check_consistency(&self.load_project(project_id)?))
    }

    /// 校验并提交一次小说记忆变更。冲突事实不会进入 Canon，其余事实原子提交。
    #[cfg(test)]
    pub fn commit_delta(&self, delta: &NovelMemoryDelta) -> Result<CommitReport> {
        self.commit_delta_internal(delta, None)
    }

    /// 以发布事务 ID 幂等提交小说记忆变更。
    pub fn commit_delta_for_publication(
        &self,
        delta: &NovelMemoryDelta,
        publication_id: &str,
    ) -> Result<CommitReport> {
        validate_project_id(publication_id)?;
        self.commit_delta_internal(delta, Some(publication_id))
    }

    fn commit_delta_internal(
        &self,
        delta: &NovelMemoryDelta,
        publication_id: Option<&str>,
    ) -> Result<CommitReport> {
        validate_delta(delta)?;
        let mut project = self.load_project(&delta.project_id)?;
        if publication_id.is_some_and(|id| {
            project
                .applied_publications
                .iter()
                .any(|applied| applied == id)
        }) {
            return Ok(CommitReport {
                project_id: project.project_id,
                previous_revision: project.canon_revision.saturating_sub(1),
                new_revision: project.canon_revision,
                accepted_fact_ids: Vec::new(),
                conflicts: Vec::new(),
                graph_mirrored: true,
            });
        }
        if delta.expected_revision != project.canon_revision {
            return Err(MemoryError::Conflict(format!(
                "小说项目 revision 已变化: expected={}, actual={}",
                delta.expected_revision, project.canon_revision
            )));
        }
        if delta.branch_id != project.active_branch {
            return Err(MemoryError::Conflict(format!(
                "变更分支 {} 与当前分支 {} 不一致",
                delta.branch_id, project.active_branch
            )));
        }
        if delta
            .progress
            .as_ref()
            .and_then(|progress| progress.current_chapter)
            .zip(project.current_chapter)
            .is_some_and(|(next, current)| next < current)
        {
            return Err(MemoryError::Conflict(
                "current_chapter 不允许在同一分支中倒退；重写请创建新分支或显式调整项目".into(),
            ));
        }

        let duplicate_ids = duplicate_fact_ids(delta);
        if let Some(id) = duplicate_ids.into_iter().next() {
            return Err(MemoryError::Conflict(format!(
                "变更集中 fact_id 重复: {id}"
            )));
        }

        let previous_revision = project.canon_revision;
        let proposed_revision = previous_revision + 1;
        let now = Utc::now().timestamp_millis();
        let mut accepted_fact_ids = Vec::new();
        let mut conflicts = Vec::new();
        let mut reinforced_any = false;
        let explicitly_superseded = delta
            .state_changes
            .iter()
            .filter_map(|change| change.supersedes_fact_id.as_deref())
            .chain(
                delta
                    .foreshadowing_updates
                    .iter()
                    .filter_map(|update| update.resolves_fact_id.as_deref()),
            )
            .collect::<HashSet<_>>();

        for raw_proposed in delta.all_facts() {
            let proposed = promoted_experience(delta, raw_proposed);
            if project
                .facts
                .iter()
                .any(|fact| fact.fact_id == proposed.fact_id)
            {
                conflicts.push(ConflictRecord {
                    conflict_id: conflict_id(&project.project_id, &proposed, now),
                    subject_key: proposed.subject_key.clone(),
                    existing_fact_id: proposed.fact_id.clone(),
                    proposed_fact_id: proposed.fact_id.clone(),
                    reason: "fact_id 已存在".into(),
                    created_at: now,
                    resolved: false,
                    resolution: None,
                });
                continue;
            }
            if reinforce_matching_experience(
                &mut project,
                &proposed,
                &delta.branch_id,
                &delta.source_ref,
                proposed_revision,
                now,
            ) {
                reinforced_any = true;
                accepted_fact_ids.push(proposed.fact_id.clone());
                continue;
            }
            if let Some(existing) = conflicting_canon(
                &project,
                &proposed,
                &delta.branch_id,
                &explicitly_superseded,
            ) {
                conflicts.push(ConflictRecord {
                    conflict_id: conflict_id(&project.project_id, &proposed, now),
                    subject_key: proposed.subject_key.clone(),
                    existing_fact_id: existing.fact_id.clone(),
                    proposed_fact_id: proposed.fact_id.clone(),
                    reason: "与当前生效 Canon 的同一 subject_key 内容冲突".into(),
                    created_at: now,
                    resolved: false,
                    resolution: None,
                });
                continue;
            }
            project.facts.push(to_fact(
                &proposed,
                &delta.branch_id,
                &delta.source_ref,
                proposed_revision,
                now,
            ));
            accepted_fact_ids.push(proposed.fact_id.clone());
        }

        let accepted = accepted_fact_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut superseded_any = false;
        for state_change in &delta.state_changes {
            if accepted.contains(state_change.fact.fact_id.as_str()) {
                superseded_any |= supersede_if_requested(
                    &mut project,
                    state_change.supersedes_fact_id.as_deref(),
                    now,
                );
            }
        }
        for update in &delta.foreshadowing_updates {
            if accepted.contains(update.fact.fact_id.as_str()) {
                superseded_any |=
                    supersede_if_requested(&mut project, update.resolves_fact_id.as_deref(), now);
            }
        }

        let progress_changed = apply_progress(&mut project, delta.progress.as_ref());
        let canon_changed =
            !accepted_fact_ids.is_empty() || reinforced_any || superseded_any || progress_changed;
        let has_new_conflicts = !conflicts.is_empty();
        project.conflicts.extend(conflicts.iter().cloned());
        let new_revision = if canon_changed || publication_id.is_some() {
            if canon_changed {
                project.canon_revision = proposed_revision;
            }
            if let Some(publication_id) = publication_id {
                project
                    .applied_publications
                    .push(publication_id.to_string());
            }
            project.updated_at = now;
            self.write_project(&project)?;
            project.canon_revision
        } else {
            if has_new_conflicts {
                project.updated_at = now;
                self.write_project(&project)?;
            }
            previous_revision
        };
        let graph_mirrored = !canon_changed || self.mirror_to_graph(&project).is_ok();

        Ok(CommitReport {
            project_id: project.project_id,
            previous_revision,
            new_revision,
            accepted_fact_ids,
            conflicts,
            graph_mirrored,
        })
    }

    fn project_path(&self, project_id: &str) -> PathBuf {
        self.root.join(format!("{project_id}.json"))
    }

    fn write_project(&self, project: &NovelProject) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.project_path(&project.project_id);
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(project)?)?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        std::fs::rename(temp, path)?;
        Ok(())
    }

    fn mirror_to_graph(&self, project: &NovelProject) -> Result<()> {
        let Some(path) = &self.graph_db_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let graph = GraphStore::open(path).map_err(graph_error)?;
        let now = Utc::now().timestamp_millis();
        let project_node_id = format!("novel_entity_project_{}", project.project_id);
        graph
            .upsert_node(&Node {
                id: project_node_id.clone(),
                kind: NodeKind::Entity,
                graph_type: GraphType::Novel,
                props: HashMap::from([
                    ("name".into(), json!(project.title)),
                    ("title".into(), json!(project.title)),
                    ("project_id".into(), json!(project.project_id)),
                    ("novel_kind".into(), json!("project")),
                    ("catalog_type".into(), json!("novel_project")),
                    (
                        "catalog_keywords".into(),
                        json!([project.project_id, project.title]),
                    ),
                    (
                        "summary".into(),
                        json!(format!("小说项目：{}", project.title)),
                    ),
                ]),
                importance: 1.0,
                created_at: project.created_at,
                last_accessed: now,
                superseded: false,
            })
            .map_err(graph_error)?;

        for fact in &project.facts {
            let node_id = graph_fact_id(&fact.fact_id);
            graph
                .upsert_node(&fact_node(project, fact, &node_id))
                .map_err(graph_error)?;
            graph
                .insert_edge(&Edge {
                    src: project_node_id.clone(),
                    dst: node_id,
                    kind: EdgeKind::Contains,
                    props: HashMap::from([("project_id".into(), json!(project.project_id))]),
                    created_at: now,
                    weight: fact.confidence.clamp(0.0, 1.0),
                })
                .map_err(graph_error)?;
        }
        Ok(())
    }
}

fn validate_project_id(project_id: &str) -> Result<()> {
    let valid = !project_id.is_empty()
        && project_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if valid {
        Ok(())
    } else {
        Err(MemoryError::Conflict(format!(
            "project_id 只能包含字母、数字、-、_: {project_id}"
        )))
    }
}

fn validate_delta(delta: &NovelMemoryDelta) -> Result<()> {
    validate_project_id(&delta.project_id)?;
    if delta.branch_id.trim().is_empty() {
        return Err(MemoryError::Conflict("branch_id 不能为空".into()));
    }
    for fact in delta.all_facts() {
        if fact.fact_id.trim().is_empty()
            || fact.subject_key.trim().is_empty()
            || fact.title.trim().is_empty()
        {
            return Err(MemoryError::Conflict(
                "fact_id、subject_key 和 title 均不能为空".into(),
            ));
        }
        if !fact.confidence.is_finite() || !(0.0..=1.0).contains(&fact.confidence) {
            return Err(MemoryError::Conflict(format!(
                "事实 {} 的 confidence 必须在 0..=1",
                fact.fact_id
            )));
        }
        if fact
            .valid_from_chapter
            .zip(fact.valid_to_chapter)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(MemoryError::Conflict(format!(
                "事实 {} 的章节有效区间无效",
                fact.fact_id
            )));
        }
    }
    Ok(())
}

fn duplicate_fact_ids(delta: &NovelMemoryDelta) -> HashSet<String> {
    let mut seen = HashSet::new();
    delta
        .all_facts()
        .map(|fact| fact.fact_id.clone())
        .filter(|id| !seen.insert(id.clone()))
        .collect()
}

fn promoted_experience(delta: &NovelMemoryDelta, proposed: &ProposedFact) -> ProposedFact {
    let mut fact = proposed.clone();
    if fact.kind == super::schema::NovelFactKind::WritingExperience
        && fact.status == CanonStatus::Draft
        && delta.experience_candidates.iter().any(|candidate| {
            candidate.fact.fact_id == fact.fact_id && candidate.evidence_count >= 2
        })
    {
        fact.status = CanonStatus::Confirmed;
    }
    fact
}

fn reinforce_matching_experience(
    project: &mut NovelProject,
    proposed: &ProposedFact,
    branch_id: &str,
    source_ref: &str,
    revision: u64,
    now: i64,
) -> bool {
    if proposed.kind != super::schema::NovelFactKind::WritingExperience {
        return false;
    }
    let Some(existing) = project.facts.iter_mut().find(|fact| {
        fact.kind == super::schema::NovelFactKind::WritingExperience
            && fact.branch_id == branch_id
            && fact.status == CanonStatus::Confirmed
            && fact.subject_key == proposed.subject_key
            && fact.summary == proposed.summary
    }) else {
        return false;
    };
    let evidence = proposed.confidence * 0.25;
    existing.confidence = 1.0 - ((1.0 - existing.confidence) * (1.0 - evidence));
    for source in proposed
        .source_refs
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(source_ref))
        .filter(|source| !source.is_empty())
    {
        if !existing.source_refs.iter().any(|item| item == source) {
            existing.source_refs.push(source.to_owned());
        }
    }
    existing.revision = revision;
    existing.updated_at = now;
    true
}

fn apply_progress(
    project: &mut NovelProject,
    progress: Option<&super::schema::NovelProjectProgress>,
) -> bool {
    let Some(progress) = progress else {
        return false;
    };
    let mut changed = false;
    if let Some(volume) = &progress.current_volume {
        if project.current_volume.as_ref() != Some(volume) {
            project.current_volume = Some(volume.clone());
            changed = true;
        }
    }
    if let Some(chapter) = progress.current_chapter {
        if project.current_chapter != Some(chapter) {
            project.current_chapter = Some(chapter);
            changed = true;
        }
    }
    changed
}

fn conflict_id(project_id: &str, proposed: &ProposedFact, now: i64) -> String {
    format!(
        "conflict-{:016x}",
        stable_hash(&format!(
            "{project_id}:{}:{}:{now}",
            proposed.subject_key, proposed.fact_id
        ))
    )
}

fn conflicting_canon<'a>(
    project: &'a NovelProject,
    proposed: &ProposedFact,
    branch_id: &str,
    explicitly_superseded: &HashSet<&str>,
) -> Option<&'a NovelFact> {
    if proposed.status != CanonStatus::Confirmed {
        return None;
    }
    project.facts.iter().find(|existing| {
        existing.branch_id == branch_id
            && existing.status == CanonStatus::Confirmed
            && !explicitly_superseded.contains(existing.fact_id.as_str())
            && existing.subject_key == proposed.subject_key
            && overlaps(
                existing.valid_from_chapter,
                existing.valid_to_chapter,
                proposed.valid_from_chapter,
                proposed.valid_to_chapter,
            )
            && (existing.summary != proposed.summary || existing.data != proposed.data)
    })
}

fn overlaps(
    a_start: Option<u32>,
    a_end: Option<u32>,
    b_start: Option<u32>,
    b_end: Option<u32>,
) -> bool {
    let a_start = a_start.unwrap_or(0);
    let a_end = a_end.unwrap_or(u32::MAX);
    let b_start = b_start.unwrap_or(0);
    let b_end = b_end.unwrap_or(u32::MAX);
    a_start <= b_end && b_start <= a_end
}

fn to_fact(
    proposed: &ProposedFact,
    branch_id: &str,
    delta_source: &str,
    revision: u64,
    now: i64,
) -> NovelFact {
    let mut source_refs = proposed.source_refs.clone();
    if !delta_source.is_empty() && !source_refs.iter().any(|item| item == delta_source) {
        source_refs.push(delta_source.to_owned());
    }
    NovelFact {
        fact_id: proposed.fact_id.clone(),
        kind: proposed.kind.clone(),
        subject_key: proposed.subject_key.clone(),
        title: proposed.title.clone(),
        summary: proposed.summary.clone(),
        data: proposed.data.clone(),
        status: proposed.status.clone(),
        branch_id: branch_id.to_owned(),
        valid_from_chapter: proposed.valid_from_chapter,
        valid_to_chapter: proposed.valid_to_chapter,
        source_refs,
        confidence: proposed.confidence.clamp(0.0, 1.0),
        revision,
        created_at: now,
        updated_at: now,
    }
}

fn supersede_if_requested(project: &mut NovelProject, fact_id: Option<&str>, now: i64) -> bool {
    if let Some(fact_id) = fact_id {
        if let Some(fact) = project
            .facts
            .iter_mut()
            .find(|fact| fact.fact_id == fact_id)
        {
            if fact.status == CanonStatus::Superseded {
                return false;
            }
            fact.status = CanonStatus::Superseded;
            fact.updated_at = now;
            return true;
        }
    }
    false
}

fn graph_fact_id(fact_id: &str) -> String {
    if fact_id.starts_with("novel_") {
        fact_id.to_owned()
    } else {
        format!("novel_memory_{:016x}", stable_hash(fact_id))
    }
}

fn stable_hash(value: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    value.as_bytes().iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

fn fact_node(project: &NovelProject, fact: &NovelFact, node_id: &str) -> Node {
    Node {
        id: node_id.to_owned(),
        kind: NodeKind::Memory,
        graph_type: GraphType::Novel,
        props: HashMap::from([
            ("name".into(), json!(fact.title)),
            ("title".into(), json!(fact.title)),
            ("summary".into(), json!(fact.summary)),
            ("project_id".into(), json!(project.project_id)),
            ("fact_id".into(), json!(fact.fact_id)),
            ("novel_kind".into(), json!(fact.kind)),
            ("catalog_type".into(), json!("novel_fact")),
            (
                "catalog_keywords".into(),
                json!([
                    project.project_id,
                    project.title,
                    fact.subject_key,
                    fact.title
                ]),
            ),
            ("source_refs".into(), json!(fact.source_refs)),
            ("data".into(), fact.data.clone()),
            ("status".into(), json!(fact.status)),
            ("branch_id".into(), json!(fact.branch_id)),
            ("revision".into(), json!(fact.revision)),
        ]),
        importance: fact.confidence.clamp(0.0, 1.0),
        created_at: fact.created_at,
        last_accessed: fact.updated_at,
        superseded: fact.status == CanonStatus::Superseded,
    }
}

fn graph_error(error: impl std::fmt::Display) -> MemoryError {
    MemoryError::Io(std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tempfile::tempdir;

    use super::*;
    use crate::novel::{ExperienceCandidate, NovelFactKind, NovelProjectStatus, StateChange};

    fn fact(id: &str, subject: &str, summary: &str) -> ProposedFact {
        ProposedFact {
            fact_id: id.into(),
            kind: NovelFactKind::CharacterState,
            subject_key: subject.into(),
            title: subject.into(),
            summary: summary.into(),
            data: Value::Null,
            status: CanonStatus::Confirmed,
            valid_from_chapter: Some(1),
            valid_to_chapter: None,
            source_refs: Vec::new(),
            confidence: 0.9,
        }
    }

    fn delta(project_id: &str, revision: u64, facts: Vec<ProposedFact>) -> NovelMemoryDelta {
        NovelMemoryDelta {
            project_id: project_id.into(),
            branch_id: "main".into(),
            expected_revision: revision,
            task_type: NovelTaskType::Body,
            source_ref: "chapters/0001.md".into(),
            progress: None,
            proposed_facts: facts,
            state_changes: Vec::new(),
            plot_updates: Vec::new(),
            foreshadowing_updates: Vec::new(),
            feedback: Vec::new(),
            experience_candidates: Vec::new(),
        }
    }

    #[test]
    fn project_is_isolated_and_delta_is_committed() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        let mut project = NovelProject::new("dark-city", "暗城");
        project.status = NovelProjectStatus::Writing;
        store.create_project(&project).unwrap();

        let report = store
            .commit_delta(&delta(
                "dark-city",
                0,
                vec![fact(
                    "linmo-location-1",
                    "character:linmo:location",
                    "林默位于旧港",
                )],
            ))
            .unwrap();
        assert_eq!(report.new_revision, 1);
        assert_eq!(report.accepted_fact_ids, vec!["linmo-location-1"]);
        assert_eq!(store.load_project("dark-city").unwrap().facts.len(), 1);
    }

    #[test]
    fn conflicting_canon_is_reported_not_overwritten() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        store
            .create_project(&NovelProject::new("dark-city", "暗城"))
            .unwrap();
        store
            .commit_delta(&delta(
                "dark-city",
                0,
                vec![fact(
                    "location-a",
                    "character:linmo:location",
                    "林默位于旧港",
                )],
            ))
            .unwrap();
        let report = store
            .commit_delta(&delta(
                "dark-city",
                1,
                vec![fact(
                    "location-b",
                    "character:linmo:location",
                    "林默位于王城",
                )],
            ))
            .unwrap();
        assert!(report.accepted_fact_ids.is_empty());
        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.new_revision, 1);
        let project = store.load_project("dark-city").unwrap();
        assert_eq!(project.facts.len(), 1);
        assert_eq!(project.facts[0].summary, "林默位于旧港");
        assert_eq!(project.conflicts.len(), 1);
        assert!(!project.conflicts[0].resolved);
        store
            .resolve_conflict(
                "dark-city",
                &project.conflicts[0].conflict_id,
                "保留旧港设定",
            )
            .unwrap();
        assert!(store.load_project("dark-city").unwrap().conflicts[0].resolved);
    }

    #[test]
    fn stale_revision_is_rejected() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        store
            .create_project(&NovelProject::new("dark-city", "暗城"))
            .unwrap();
        let error = store
            .commit_delta(&delta("dark-city", 3, Vec::new()))
            .unwrap_err();
        assert!(error.to_string().contains("revision"));
    }

    #[test]
    fn explicit_state_change_supersedes_old_canon() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        store
            .create_project(&NovelProject::new("dark-city", "暗城"))
            .unwrap();
        store
            .commit_delta(&delta(
                "dark-city",
                0,
                vec![fact(
                    "location-a",
                    "character:linmo:location",
                    "林默位于旧港",
                )],
            ))
            .unwrap();
        let mut update = delta("dark-city", 1, Vec::new());
        update.state_changes.push(StateChange {
            fact: fact("location-b", "character:linmo:location", "林默抵达王城"),
            supersedes_fact_id: Some("location-a".into()),
        });
        let report = store.commit_delta(&update).unwrap();
        assert_eq!(report.accepted_fact_ids, vec!["location-b"]);
        assert!(report.conflicts.is_empty());
        let project = store.load_project("dark-city").unwrap();
        assert_eq!(project.facts.len(), 2);
        assert_eq!(project.facts[0].status, CanonStatus::Superseded);
        assert_eq!(project.facts[1].status, CanonStatus::Confirmed);
    }

    #[test]
    fn body_recall_only_returns_confirmed_active_facts() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        let mut project = NovelProject::new("dark-city", "暗城");
        project.current_chapter = Some(2);
        store.create_project(&project).unwrap();
        store
            .commit_delta(&delta(
                "dark-city",
                0,
                vec![fact(
                    "location-a",
                    "character:linmo:location",
                    "林默位于旧港",
                )],
            ))
            .unwrap();
        let pack = store.recall("dark-city", NovelTaskType::Body).unwrap();
        assert_eq!(pack.facts.len(), 1);
        assert!(pack.rendered_context.contains("林默位于旧港"));
    }

    #[test]
    fn continuation_recall_includes_events_and_active_plot_threads() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        store
            .create_project(&NovelProject::new("dark-city", "暗城"))
            .unwrap();
        let mut event = fact("event-1", "event:old-port-fire", "旧港发生大火");
        event.kind = NovelFactKind::Event;
        let mut plot = fact("plot-1", "plot:missing-key", "铜钥匙仍未找到");
        plot.kind = NovelFactKind::PlotThread;
        store
            .commit_delta(&delta("dark-city", 0, vec![event, plot]))
            .unwrap();

        let pack = store
            .recall("dark-city", NovelTaskType::Continuation)
            .unwrap();
        assert!(pack
            .facts
            .iter()
            .any(|fact| fact.kind == NovelFactKind::Event));
        assert!(pack
            .facts
            .iter()
            .any(|fact| fact.kind == NovelFactKind::PlotThread));
    }

    #[test]
    fn progress_is_committed_with_revision() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        store
            .create_project(&NovelProject::new("dark-city", "暗城"))
            .unwrap();
        let mut update = delta("dark-city", 0, Vec::new());
        update.progress = Some(crate::novel::NovelProjectProgress {
            current_volume: Some("第一卷".into()),
            current_chapter: Some(12),
        });
        let report = store.commit_delta(&update).unwrap();
        assert_eq!(report.new_revision, 1);
        let project = store.load_project("dark-city").unwrap();
        assert_eq!(project.current_volume.as_deref(), Some("第一卷"));
        assert_eq!(project.current_chapter, Some(12));
    }

    #[test]
    fn repeated_experience_is_promoted_and_reinforced() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        store
            .create_project(&NovelProject::new("dark-city", "暗城"))
            .unwrap();
        let mut experience = fact(
            "style-short-dialogue-1",
            "style:dialogue:length",
            "对白保持简短",
        );
        experience.kind = NovelFactKind::WritingExperience;
        experience.status = CanonStatus::Draft;
        let mut first = delta("dark-city", 0, Vec::new());
        first.experience_candidates.push(ExperienceCandidate {
            fact: experience,
            evidence_count: 2,
        });
        store.commit_delta(&first).unwrap();
        let project = store.load_project("dark-city").unwrap();
        assert_eq!(project.facts[0].status, CanonStatus::Confirmed);
        let initial_confidence = project.facts[0].confidence;

        let mut repeated = fact(
            "style-short-dialogue-2",
            "style:dialogue:length",
            "对白保持简短",
        );
        repeated.kind = NovelFactKind::WritingExperience;
        let mut second = delta("dark-city", 1, Vec::new());
        second.experience_candidates.push(ExperienceCandidate {
            fact: repeated,
            evidence_count: 1,
        });
        store.commit_delta(&second).unwrap();
        let project = store.load_project("dark-city").unwrap();
        assert_eq!(project.facts.len(), 1);
        assert!(project.facts[0].confidence > initial_confidence);
        assert_eq!(project.canon_revision, 2);
    }

    #[test]
    fn publication_commit_is_idempotent() {
        let dir = tempdir().unwrap();
        let store = NovelMemoryStore::new(dir.path(), None);
        store
            .create_project(&NovelProject::new("dark-city", "暗城"))
            .unwrap();
        let update = delta(
            "dark-city",
            0,
            vec![fact(
                "location-a",
                "character:linmo:location",
                "林默位于旧港",
            )],
        );

        let first = store
            .commit_delta_for_publication(&update, "publication-1")
            .unwrap();
        let retry = store
            .commit_delta_for_publication(&update, "publication-1")
            .unwrap();

        assert_eq!(first.new_revision, 1);
        assert_eq!(retry.new_revision, 1);
        let project = store.load_project("dark-city").unwrap();
        assert_eq!(project.facts.len(), 1);
        assert_eq!(project.applied_publications, vec!["publication-1"]);
    }
}
