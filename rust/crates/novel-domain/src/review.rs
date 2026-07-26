use std::collections::HashSet;

use crate::{
    MainReviewRecord, MainReviewVerdict, NovelDomainError, NovelDraftEnvelope, NovelSelfReview,
    NovelSelfReviewVerdict, Result,
};

pub fn validate_self_review(review: &NovelSelfReview) -> Result<()> {
    let issues_are_valid = review
        .issues
        .iter()
        .all(|issue| !issue.message().trim().is_empty());
    let assumptions_are_valid = review
        .unverified_assumptions
        .iter()
        .all(|item| !item.trim().is_empty());
    let pass_is_consistent = review.verdict == NovelSelfReviewVerdict::Pass
        && review.checks.all_pass()
        && review.issues.is_empty()
        && !review.summary.trim().is_empty();
    if !issues_are_valid || !assumptions_are_valid || !pass_is_consistent {
        return Err(NovelDomainError::InvalidCandidate(
            "self review requires all checks to pass, no issues, and a summary".into(),
        ));
    }
    Ok(())
}

pub fn validate_main_review(review: &MainReviewRecord, draft: &NovelDraftEnvelope) -> Result<()> {
    if review.task_id != draft.task_id
        || review.draft_version != draft.draft_version
        || review.reviewed_canon_revision != draft.canon_revision
    {
        return Err(NovelDomainError::InvalidTransition(
            "review task, draft version, or Canon revision is stale".into(),
        ));
    }
    if review.summary.trim().is_empty()
        || review
            .issues
            .iter()
            .any(|issue| issue.message().trim().is_empty())
    {
        return Err(NovelDomainError::InvalidTransition(
            "review summary and issues must be valid".into(),
        ));
    }
    match review.verdict {
        MainReviewVerdict::Pass => {
            validate_self_review(&draft.self_review)?;
            let evidence = review
                .evidence_refs
                .iter()
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .collect::<HashSet<_>>();
            if !review.checks.all_pass() || !review.issues.is_empty() || evidence.len() < 3 {
                return Err(NovelDomainError::InvalidTransition(
                    "passing review requires all checks, no issues, and three evidence sources"
                        .into(),
                ));
            }
        }
        MainReviewVerdict::Revise if review.issues.is_empty() => {
            return Err(NovelDomainError::InvalidTransition(
                "revision review requires concrete issues".into(),
            ));
        }
        MainReviewVerdict::Revise => {}
    }
    Ok(())
}
