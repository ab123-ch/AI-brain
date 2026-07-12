use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::schema::{CanonStatus, NovelFact, NovelFactKind, NovelProject};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyIssue {
    pub code: String,
    pub severity: IssueSeverity,
    pub message: String,
    #[serde(default)]
    pub fact_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyReport {
    pub project_id: String,
    pub revision: u64,
    pub checked_chapter: Option<u32>,
    pub issues: Vec<ConsistencyIssue>,
}

impl ConsistencyReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.issues
            .iter()
            .any(|issue| issue.severity == IssueSeverity::Error)
    }
}

#[must_use]
pub fn check_consistency(project: &NovelProject) -> ConsistencyReport {
    let mut issues = Vec::new();
    for conflict in project.conflicts.iter().filter(|item| !item.resolved) {
        issues.push(ConsistencyIssue {
            code: "unresolved_canon_conflict".into(),
            severity: IssueSeverity::Error,
            message: format!(
                "未处理 Canon 冲突：{}（{}）",
                conflict.subject_key, conflict.reason
            ),
            fact_ids: vec![
                conflict.existing_fact_id.clone(),
                conflict.proposed_fact_id.clone(),
            ],
        });
    }

    let active = project
        .facts
        .iter()
        .filter(|fact| {
            fact.status == CanonStatus::Confirmed && fact.branch_id == project.active_branch
        })
        .collect::<Vec<_>>();
    let mut by_subject: HashMap<&str, Vec<&NovelFact>> = HashMap::new();
    for fact in &active {
        by_subject
            .entry(fact.subject_key.as_str())
            .or_default()
            .push(fact);
    }
    for (subject, facts) in by_subject {
        for (index, left) in facts.iter().enumerate() {
            for right in facts.iter().skip(index + 1) {
                if overlaps(left, right)
                    && (left.summary != right.summary || left.data != right.data)
                {
                    issues.push(ConsistencyIssue {
                        code: "overlapping_canon".into(),
                        severity: IssueSeverity::Error,
                        message: format!("同一事实键 {subject} 存在有效期重叠的不同 Canon"),
                        fact_ids: vec![left.fact_id.clone(), right.fact_id.clone()],
                    });
                }
            }
        }
    }

    if let Some(current_chapter) = project.current_chapter {
        for fact in active
            .iter()
            .filter(|fact| fact.kind == NovelFactKind::Foreshadowing)
        {
            let target = fact
                .data
                .get("target_chapter")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok());
            if target.is_some_and(|target| target < current_chapter) {
                issues.push(ConsistencyIssue {
                    code: "overdue_foreshadowing".into(),
                    severity: IssueSeverity::Warning,
                    message: format!(
                        "伏笔“{}”计划在第 {} 章前处理，但当前已到第 {} 章",
                        fact.title,
                        target.unwrap_or_default(),
                        current_chapter
                    ),
                    fact_ids: vec![fact.fact_id.clone()],
                });
            }
        }
    }

    ConsistencyReport {
        project_id: project.project_id.clone(),
        revision: project.canon_revision,
        checked_chapter: project.current_chapter,
        issues,
    }
}

fn overlaps(left: &NovelFact, right: &NovelFact) -> bool {
    let left_start = left.valid_from_chapter.unwrap_or(0);
    let left_end = left.valid_to_chapter.unwrap_or(u32::MAX);
    let right_start = right.valid_from_chapter.unwrap_or(0);
    let right_end = right.valid_to_chapter.unwrap_or(u32::MAX);
    left_start <= right_end && right_start <= left_end
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::novel::{ConflictRecord, NovelProject};

    #[test]
    fn reports_unresolved_conflict_and_overdue_foreshadowing() {
        let mut project = NovelProject::new("dark-city", "暗城");
        project.current_chapter = Some(20);
        project.conflicts.push(ConflictRecord {
            conflict_id: "conflict-1".into(),
            subject_key: "character:linmo:location".into(),
            existing_fact_id: "old".into(),
            proposed_fact_id: "new".into(),
            reason: "地点冲突".into(),
            created_at: 1,
            resolved: false,
            resolution: None,
        });
        let now = chrono::Utc::now().timestamp_millis();
        project.facts.push(NovelFact {
            fact_id: "foreshadow-key".into(),
            kind: NovelFactKind::Foreshadowing,
            subject_key: "foreshadow:key".into(),
            title: "铜钥匙".into(),
            summary: "铜钥匙来源未揭示".into(),
            data: json!({ "target_chapter": 10 }),
            status: CanonStatus::Confirmed,
            branch_id: "main".into(),
            valid_from_chapter: Some(1),
            valid_to_chapter: None,
            source_refs: vec![],
            confidence: 0.9,
            revision: 1,
            created_at: now,
            updated_at: now,
        });

        let report = check_consistency(&project);
        assert!(report.has_errors());
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "overdue_foreshadowing"));
    }
}
