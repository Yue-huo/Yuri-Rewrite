use crate::domain::{
    AppState, Chapter, NovelSettings, ParsedChapterRewrite, ReviewCoverageItem, ReviewDecision,
    ReviewIssue, RewritePlan, RewriteReviewDecision, RewriteStateUpdate,
};
use crate::{
    load_canon_asset_content, parse_jsonish_value, parse_review_decision_output, to_string,
    upsert_canon_asset, REWRITE_CONTINUITY_ASSET_KIND,
};
use chrono::Utc;
use rusqlite::params;
use std::collections::{HashMap, HashSet};
use tauri::State;

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

pub(crate) fn parse_rewrite_review_decision_output(
    output: &str,
    settings: &NovelSettings,
    plan: &RewritePlan,
    rewrites: &[ParsedChapterRewrite],
) -> Result<RewriteReviewDecision, String> {
    let mut decision = parse_review_decision_output(output, settings)?;
    let value = parse_jsonish_value(output)?;
    let coverage = value
        .get("coverage")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value::<ReviewCoverageItem>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("coverage 字段无效：{error}"))?;
    let state_updates = value
        .get("state_updates")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serde_json::from_value::<RewriteStateUpdate>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("state_updates 字段无效：{error}"))?;

    let expected = plan
        .obligations
        .iter()
        .map(|obligation| (obligation.obligation_id.as_str(), obligation))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    for item in &coverage {
        let Some(obligation) = expected.get(item.obligation_id.as_str()) else {
            decision.issues.push(ReviewIssue {
                chapter_indexes: item.chapter_indexes.clone(),
                scope: "chapter".to_string(),
                category: "obligation_coverage".to_string(),
                severity: "blocking".to_string(),
                problem: format!("coverage 引用了未知义务 {}。", item.obligation_id),
                required_fix: "仅按当前契约逐项重新验收。".to_string(),
            });
            continue;
        };
        if !seen.insert(item.obligation_id.clone()) {
            decision.issues.push(ReviewIssue {
                chapter_indexes: vec![obligation.chapter_index],
                scope: "chapter".to_string(),
                category: "obligation_coverage".to_string(),
                severity: "blocking".to_string(),
                problem: format!("义务 {} 在 coverage 中重复出现。", item.obligation_id),
                required_fix: "每个义务只能输出一个 coverage 项。".to_string(),
            });
        }
        if item.chapter_indexes.as_slice() != [obligation.chapter_index] {
            decision.issues.push(ReviewIssue {
                chapter_indexes: vec![obligation.chapter_index],
                scope: "chapter".to_string(),
                category: "obligation_coverage".to_string(),
                severity: "blocking".to_string(),
                problem: format!(
                    "义务 {} 的 coverage 章节索引必须且只能是 {}。",
                    item.obligation_id, obligation.chapter_index
                ),
                required_fix: format!(
                    "将义务 {} 的 chapter_indexes 修正为 [{}]。",
                    item.obligation_id, obligation.chapter_index
                ),
            });
        }
        if !matches!(
            item.status.as_str(),
            "satisfied" | "partial" | "missed" | "regressed"
        ) {
            return Err(format!(
                "义务 {} 使用了未知 coverage 状态：{}",
                item.obligation_id, item.status
            ));
        }
        let evidence_exists = evidence_exists_in_rewrite(item, rewrites);
        if item.status != "satisfied" || !evidence_exists {
            decision.issues.push(ReviewIssue {
                chapter_indexes: vec![obligation.chapter_index],
                scope: "chapter".to_string(),
                category: "obligation_coverage".to_string(),
                severity: "blocking".to_string(),
                problem: if item.status == "satisfied" {
                    format!(
                        "义务 {} 的 satisfied 证据无法在当前改写稿中定位。",
                        item.obligation_id
                    )
                } else {
                    format!("义务 {} 状态为 {}。", item.obligation_id, item.status)
                },
                required_fix: format!(
                    "定向完成义务 {}：{}",
                    item.obligation_id,
                    obligation.required_changes.join("；")
                ),
            });
        }
    }
    for obligation in &plan.obligations {
        if !seen.contains(&obligation.obligation_id) {
            decision.issues.push(ReviewIssue {
                chapter_indexes: vec![obligation.chapter_index],
                scope: "chapter".to_string(),
                category: "obligation_coverage".to_string(),
                severity: "blocking".to_string(),
                problem: format!("coverage 遗漏义务 {}。", obligation.obligation_id),
                required_fix: format!(
                    "验收并完成义务 {}：{}",
                    obligation.obligation_id,
                    obligation.required_changes.join("；")
                ),
            });
        }
    }

    decision
        .issues
        .extend(validate_state_updates(plan, &state_updates));
    decision.approved = coverage_gate_passes(plan, &coverage, &decision.issues);
    Ok(RewriteReviewDecision {
        decision,
        coverage,
        state_updates,
    })
}

fn state_signature(state: &RewriteStateUpdate) -> (String, String, String, i64, Vec<String>) {
    let mut source_ids = state
        .source_obligation_ids
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    source_ids.sort();
    source_ids.dedup();
    (
        state.thread_key.trim().to_string(),
        state.state_type.trim().to_string(),
        state.value.trim().to_string(),
        state.chapter_index,
        source_ids,
    )
}

fn state_issue(state: &RewriteStateUpdate, problem: String, required_fix: String) -> ReviewIssue {
    ReviewIssue {
        chapter_indexes: vec![state.chapter_index],
        scope: "cross_chapter".to_string(),
        category: "continuity".to_string(),
        severity: "blocking".to_string(),
        problem,
        required_fix,
    }
}

pub(crate) fn validate_state_updates(
    plan: &RewritePlan,
    actual: &[RewriteStateUpdate],
) -> Vec<ReviewIssue> {
    let expected = plan
        .planned_state_updates
        .iter()
        .chain(
            plan.obligations
                .iter()
                .flat_map(|obligation| obligation.planned_state_updates.iter()),
        )
        .collect::<Vec<_>>();
    let expected_signatures = expected
        .iter()
        .map(|state| state_signature(state))
        .collect::<HashSet<_>>();
    let actual_signatures = actual.iter().map(state_signature).collect::<Vec<_>>();
    let mut issues = Vec::new();
    let mut seen_actual = HashSet::new();
    for (state, signature) in actual.iter().zip(actual_signatures.iter()) {
        let normalized_source_count = state
            .source_obligation_ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect::<HashSet<_>>()
            .len();
        if normalized_source_count != state.source_obligation_ids.len() {
            issues.push(state_issue(
                state,
                format!(
                    "复检状态 {} / {} 的来源义务 ID 为空或重复。",
                    state.thread_key, state.state_type
                ),
                "source_obligation_ids 必须为非空且不重复的契约义务 ID。".to_string(),
            ));
        }
        if !seen_actual.insert(signature.clone()) {
            issues.push(state_issue(
                state,
                format!(
                    "复检重复返回状态 {} / {} / 第{}章。",
                    state.thread_key, state.state_type, state.chapter_index
                ),
                "每个计划状态只能返回一次。".to_string(),
            ));
        } else if !expected_signatures.contains(signature) {
            let same_value = expected.iter().find(|expected_state| {
                expected_state.thread_key.trim() == state.thread_key.trim()
                    && expected_state.state_type.trim() == state.state_type.trim()
                    && expected_state.value.trim() == state.value.trim()
            });
            let required_fix = same_value.map_or_else(
                || "删除未计划状态，或重新规划后再生成正文。".to_string(),
                |expected_state| {
                    format!(
                        "必须使用计划章节 {} 和来源义务 [{}]。",
                        expected_state.chapter_index,
                        expected_state.source_obligation_ids.join("、")
                    )
                },
            );
            issues.push(state_issue(
                state,
                format!(
                    "复检状态与契约不完全一致：{} / {} / 第{}章 / 来源 [{}]。",
                    state.thread_key,
                    state.state_type,
                    state.chapter_index,
                    state.source_obligation_ids.join("、")
                ),
                required_fix,
            ));
        }
    }
    let actual_set = actual_signatures.into_iter().collect::<HashSet<_>>();
    for expected_state in expected {
        let signature = state_signature(expected_state);
        if !actual_set.contains(&signature) {
            issues.push(state_issue(
                expected_state,
                format!(
                    "复检遗漏计划状态 {} / {} / 第{}章 / 来源 [{}]。",
                    expected_state.thread_key,
                    expected_state.state_type,
                    expected_state.chapter_index,
                    expected_state.source_obligation_ids.join("、")
                ),
                format!(
                    "修复正文后返回完整计划状态；来源义务必须为 [{}]。",
                    expected_state.source_obligation_ids.join("、")
                ),
            ));
        }
    }
    issues
}

pub(crate) fn merge_continuity_states(
    existing: Vec<RewriteStateUpdate>,
    updates: &[RewriteStateUpdate],
) -> Vec<RewriteStateUpdate> {
    let mut merged = existing
        .into_iter()
        .map(|state| ((state.thread_key.clone(), state.state_type.clone()), state))
        .collect::<HashMap<_, _>>();
    for update in updates {
        let key = (update.thread_key.clone(), update.state_type.clone());
        if merged
            .get(&key)
            .is_none_or(|current| update.chapter_index >= current.chapter_index)
        {
            merged.insert(key, update.clone());
        }
    }
    let mut states = merged.into_values().collect::<Vec<_>>();
    states.sort_by_key(|state| {
        (
            state.chapter_index,
            state.thread_key.clone(),
            state.state_type.clone(),
        )
    });
    states
}

pub(crate) fn persist_rewrite_review_result(
    state: &State<'_, AppState>,
    novel_id: &str,
    shard: &[Chapter],
    plan: &RewritePlan,
    decision: &ReviewDecision,
    coverage: &[ReviewCoverageItem],
    state_updates: &[RewriteStateUpdate],
) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    let coverage_json = serde_json::to_string_pretty(&serde_json::json!({
        "approved": decision.approved,
        "coverage": coverage,
        "state_updates": state_updates,
    }))
    .map_err(to_string)?;
    let obligations = plan
        .obligations
        .iter()
        .map(|obligation| (obligation.obligation_id.as_str(), obligation.chapter_index))
        .collect::<HashMap<_, _>>();
    let mut conn = state.conn.lock().map_err(to_string)?;
    let tx = conn.transaction().map_err(to_string)?;
    for chapter in shard {
        let satisfied = coverage
            .iter()
            .filter(|item| {
                item.status == "satisfied"
                    && obligations
                        .get(item.obligation_id.as_str())
                        .is_some_and(|chapter_index| *chapter_index == chapter.index)
            })
            .count();
        tx.execute(
            "UPDATE rewrite_contracts
             SET coverage_json = ?1, validation_status = ?2,
                 obligation_satisfied = ?3, updated_at = ?4
             WHERE chapter_id = ?5",
            params![
                coverage_json,
                if decision.approved {
                    "passed"
                } else {
                    "failed"
                },
                satisfied,
                now,
                chapter.id
            ],
        )
        .map_err(to_string)?;
    }
    if decision.approved {
        let existing = load_canon_asset_content(&tx, novel_id, REWRITE_CONTINUITY_ASSET_KIND)?
            .and_then(|content| serde_json::from_str::<Vec<RewriteStateUpdate>>(&content).ok())
            .unwrap_or_default();
        let states = merge_continuity_states(existing, state_updates);
        let content = serde_json::to_string_pretty(&states).map_err(to_string)?;
        upsert_canon_asset(&tx, novel_id, REWRITE_CONTINUITY_ASSET_KIND, &content, &now)
            .map_err(to_string)?;
    }
    tx.commit().map_err(to_string)
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

    #[test]
    fn state_updates_require_exact_chapter_and_source_obligation_ids() {
        let expected = RewriteStateUpdate {
            thread_key: "承诺线".to_string(),
            state_type: "边界".to_string(),
            value: "已经约定不再隐瞒".to_string(),
            chapter_index: 3,
            source_obligation_ids: vec!["O-1".to_string()],
        };
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![RewriteObligation {
                obligation_id: "O-1".to_string(),
                node_id: "N-1".to_string(),
                chapter_index: 3,
                rule_ids: Vec::new(),
                preserve: Vec::new(),
                required_changes: Vec::new(),
                deep_delta_categories: Vec::new(),
                forbidden_regressions: Vec::new(),
                downstream_effects: Vec::new(),
                planned_state_updates: vec![expected.clone()],
            }],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };

        assert!(validate_state_updates(&plan, std::slice::from_ref(&expected)).is_empty());
        let mut wrong_chapter = expected.clone();
        wrong_chapter.chapter_index = 4;
        assert!(!validate_state_updates(&plan, &[wrong_chapter]).is_empty());
        let mut wrong_source = expected;
        wrong_source.source_obligation_ids = vec!["O-other".to_string()];
        assert!(!validate_state_updates(&plan, &[wrong_source]).is_empty());
    }

    #[test]
    fn continuity_merge_keeps_latest_state_and_its_provenance() {
        let old = RewriteStateUpdate {
            thread_key: "承诺线".to_string(),
            state_type: "边界".to_string(),
            value: "旧状态".to_string(),
            chapter_index: 2,
            source_obligation_ids: vec!["O-old".to_string()],
        };
        let latest = RewriteStateUpdate {
            thread_key: "承诺线".to_string(),
            state_type: "边界".to_string(),
            value: "最新状态".to_string(),
            chapter_index: 5,
            source_obligation_ids: vec!["O-new".to_string()],
        };

        let merged = merge_continuity_states(vec![old], std::slice::from_ref(&latest));

        assert_eq!(merged, vec![latest]);
    }
}
