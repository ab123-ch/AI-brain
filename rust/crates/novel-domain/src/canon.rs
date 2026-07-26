use std::collections::HashSet;

use crate::{
    CanonStatus, CommitReport, ConflictRecord, NovelDomainError, NovelFact, NovelFactKind,
    NovelMemoryDelta, NovelProject, ProposedFact, Result,
};

#[derive(Debug, Clone, PartialEq)]
pub struct CanonCommitOutcome {
    pub project: NovelProject,
    pub report: CommitReport,
    pub should_persist: bool,
    pub canon_changed: bool,
}

pub fn apply_canon_delta(
    project: &NovelProject,
    delta: &NovelMemoryDelta,
    publication_id: Option<&str>,
    now: i64,
) -> Result<CanonCommitOutcome> {
    validate_delta(delta)?;
    project.validate()?;
    if publication_id.is_some_and(|id| {
        project
            .applied_publications
            .iter()
            .any(|applied| applied == id)
    }) {
        return Ok(CanonCommitOutcome {
            project: project.clone(),
            report: CommitReport {
                project_id: project.project_id.clone(),
                previous_revision: project.canon_revision.saturating_sub(1),
                new_revision: project.canon_revision,
                accepted_fact_ids: Vec::new(),
                conflicts: Vec::new(),
                graph_mirrored: true,
            },
            should_persist: false,
            canon_changed: false,
        });
    }
    validate_delta_for_project(project, delta)?;

    let mut next = project.clone();
    let previous_revision = next.canon_revision;
    let proposed_revision = previous_revision + 1;
    let AppliedFacts {
        accepted_fact_ids,
        conflicts,
        reinforced_any,
    } = apply_facts(&mut next, delta, proposed_revision, now);
    let superseded_any = apply_supersessions(&mut next, delta, &accepted_fact_ids, now);
    let progress_changed = apply_progress(&mut next, delta.progress.as_ref());
    let canon_changed =
        !accepted_fact_ids.is_empty() || reinforced_any || superseded_any || progress_changed;
    next.conflicts.extend(conflicts.iter().cloned());
    let should_persist = canon_changed || !conflicts.is_empty() || publication_id.is_some();
    if canon_changed {
        next.canon_revision = proposed_revision;
    }
    if let Some(publication_id) = publication_id {
        next.applied_publications.push(publication_id.to_string());
    }
    if should_persist {
        next.updated_at = now;
    }
    let new_revision = next.canon_revision;
    Ok(CanonCommitOutcome {
        project: next,
        report: CommitReport {
            project_id: project.project_id.clone(),
            previous_revision,
            new_revision,
            accepted_fact_ids,
            conflicts,
            graph_mirrored: !canon_changed,
        },
        should_persist,
        canon_changed,
    })
}

fn validate_delta_for_project(project: &NovelProject, delta: &NovelMemoryDelta) -> Result<()> {
    if delta.expected_revision != project.canon_revision {
        return Err(NovelDomainError::StaleRevision {
            expected: delta.expected_revision,
            actual: project.canon_revision,
        });
    }
    if delta.project_id != project.project_id {
        return Err(NovelDomainError::InvalidRequest(
            "delta project does not match the aggregate".into(),
        ));
    }
    if delta.branch_id != project.active_branch {
        return Err(NovelDomainError::InvalidTransition(format!(
            "delta branch {} does not match active branch {}",
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
        return Err(NovelDomainError::InvalidTransition(
            "chapter progress cannot move backwards on the active branch".into(),
        ));
    }
    if let Some(id) = duplicate_fact_ids(delta).into_iter().next() {
        return Err(NovelDomainError::InvalidRequest(format!(
            "duplicate fact id in delta: {id}"
        )));
    }
    Ok(())
}

struct AppliedFacts {
    accepted_fact_ids: Vec<String>,
    conflicts: Vec<ConflictRecord>,
    reinforced_any: bool,
}

fn apply_facts(
    project: &mut NovelProject,
    delta: &NovelMemoryDelta,
    revision: u64,
    now: i64,
) -> AppliedFacts {
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
    let mut applied = AppliedFacts {
        accepted_fact_ids: Vec::new(),
        conflicts: Vec::new(),
        reinforced_any: false,
    };
    for raw_proposed in delta.all_facts() {
        let proposed = promoted_experience(delta, raw_proposed);
        if project
            .facts
            .iter()
            .any(|fact| fact.fact_id == proposed.fact_id)
        {
            applied.conflicts.push(conflict(
                &project.project_id,
                &proposed,
                &proposed.fact_id,
                "fact_id already exists",
                now,
            ));
            continue;
        }
        if reinforce_matching_experience(
            project,
            &proposed,
            &delta.branch_id,
            &delta.source_ref,
            revision,
            now,
        ) {
            applied.reinforced_any = true;
            applied.accepted_fact_ids.push(proposed.fact_id.clone());
            continue;
        }
        if let Some(existing) =
            conflicting_canon(project, &proposed, &delta.branch_id, &explicitly_superseded)
        {
            applied.conflicts.push(conflict(
                &project.project_id,
                &proposed,
                &existing.fact_id,
                "conflicts with active Canon for the same subject",
                now,
            ));
            continue;
        }
        project.facts.push(to_fact(
            &proposed,
            &delta.branch_id,
            &delta.source_ref,
            revision,
            now,
        ));
        applied.accepted_fact_ids.push(proposed.fact_id.clone());
    }
    applied
}

fn apply_supersessions(
    project: &mut NovelProject,
    delta: &NovelMemoryDelta,
    accepted_fact_ids: &[String],
    now: i64,
) -> bool {
    let accepted = accepted_fact_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut changed = false;
    for state_change in &delta.state_changes {
        if accepted.contains(state_change.fact.fact_id.as_str()) {
            changed |=
                supersede_if_requested(project, state_change.supersedes_fact_id.as_deref(), now);
        }
    }
    for update in &delta.foreshadowing_updates {
        if accepted.contains(update.fact.fact_id.as_str()) {
            changed |= supersede_if_requested(project, update.resolves_fact_id.as_deref(), now);
        }
    }
    changed
}

pub fn resolve_conflict(
    project: &NovelProject,
    conflict_id: &str,
    resolution: &str,
    now: i64,
) -> Result<NovelProject> {
    if resolution.trim().is_empty() {
        return Err(NovelDomainError::InvalidRequest(
            "conflict resolution must not be empty".into(),
        ));
    }
    let mut next = project.clone();
    let conflict = next
        .conflicts
        .iter_mut()
        .find(|item| item.conflict_id == conflict_id)
        .ok_or_else(|| {
            NovelDomainError::InvalidRequest(format!("conflict not found: {conflict_id}"))
        })?;
    conflict.resolved = true;
    conflict.resolution = Some(resolution.into());
    next.updated_at = now;
    Ok(next)
}

fn validate_delta(delta: &NovelMemoryDelta) -> Result<()> {
    if delta.project_id.trim().is_empty() || delta.branch_id.trim().is_empty() {
        return Err(NovelDomainError::InvalidRequest(
            "delta project and branch are required".into(),
        ));
    }
    for fact in delta.all_facts() {
        if fact.fact_id.trim().is_empty()
            || fact.subject_key.trim().is_empty()
            || fact.title.trim().is_empty()
        {
            return Err(NovelDomainError::InvalidRequest(
                "fact id, subject key, and title are required".into(),
            ));
        }
        if !fact.confidence.is_finite() || !(0.0..=1.0).contains(&fact.confidence) {
            return Err(NovelDomainError::InvalidRequest(format!(
                "fact {} confidence must be within 0..=1",
                fact.fact_id
            )));
        }
        if fact
            .valid_from_chapter
            .zip(fact.valid_to_chapter)
            .is_some_and(|(start, end)| start > end)
        {
            return Err(NovelDomainError::InvalidRequest(format!(
                "fact {} has an invalid chapter interval",
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
    if fact.kind == NovelFactKind::WritingExperience
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
    if proposed.kind != NovelFactKind::WritingExperience {
        return false;
    }
    let Some(existing) = project.facts.iter_mut().find(|fact| {
        fact.kind == NovelFactKind::WritingExperience
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
    progress: Option<&crate::NovelProjectProgress>,
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

fn conflict(
    project_id: &str,
    proposed: &ProposedFact,
    existing_fact_id: &str,
    reason: &str,
    now: i64,
) -> ConflictRecord {
    ConflictRecord {
        conflict_id: format!(
            "conflict-{:016x}",
            stable_hash(&format!(
                "{project_id}:{}:{}:{now}",
                proposed.subject_key, proposed.fact_id
            ))
        ),
        subject_key: proposed.subject_key.clone(),
        existing_fact_id: existing_fact_id.into(),
        proposed_fact_id: proposed.fact_id.clone(),
        reason: reason.into(),
        created_at: now,
        resolved: false,
        resolution: None,
    }
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
    let Some(fact_id) = fact_id else {
        return false;
    };
    let Some(fact) = project
        .facts
        .iter_mut()
        .find(|fact| fact.fact_id == fact_id)
    else {
        return false;
    };
    if fact.status == CanonStatus::Superseded {
        return false;
    }
    fact.status = CanonStatus::Superseded;
    fact.updated_at = now;
    true
}

fn stable_hash(value: &str) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    value.as_bytes().iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}
