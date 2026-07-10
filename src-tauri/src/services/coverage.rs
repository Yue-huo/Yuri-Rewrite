use crate::domain::{ParsedChapterRewrite, ReviewCoverageItem, ReviewIssue, RewritePlan};
use std::collections::HashSet;

pub(crate) fn evidence_exists_in_rewrite(
    item: &ReviewCoverageItem,
    rewrites: &[ParsedChapterRewrite],
) -> bool {
    let evidence = item.evidence.trim();
    !evidence.is_empty()
        && rewrites.iter().any(|rewrite| {
            (item.chapter_indexes.is_empty() || item.chapter_indexes.contains(&rewrite.index))
                && (rewrite.title.contains(evidence) || rewrite.text.contains(evidence))
        })
}

pub(crate) fn coverage_gate_passes(
    plan: &RewritePlan,
    coverage: &[ReviewCoverageItem],
    issues: &[ReviewIssue],
) -> bool {
    if !issues.is_empty() || coverage.len() != plan.obligations.len() {
        return false;
    }
    let expected = plan
        .obligations
        .iter()
        .map(|obligation| obligation.obligation_id.as_str())
        .collect::<HashSet<_>>();
    let actual = coverage
        .iter()
        .filter(|item| item.status == "satisfied")
        .map(|item| item.obligation_id.as_str())
        .collect::<HashSet<_>>();
    actual == expected && actual.len() == coverage.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RewriteObligation;

    #[test]
    fn gate_rejects_partial_or_duplicate_coverage() {
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![RewriteObligation {
                obligation_id: "O-1".to_string(),
                node_id: "N-1".to_string(),
                chapter_index: 1,
                rule_ids: Vec::new(),
                preserve: Vec::new(),
                required_changes: Vec::new(),
                deep_delta_categories: Vec::new(),
                forbidden_regressions: Vec::new(),
                downstream_effects: Vec::new(),
                planned_state_updates: Vec::new(),
            }],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };
        let partial = vec![ReviewCoverageItem {
            obligation_id: "O-1".to_string(),
            status: "partial".to_string(),
            chapter_indexes: vec![1],
            evidence: "证据".to_string(),
        }];
        assert!(!coverage_gate_passes(&plan, &partial, &[]));
    }
}
