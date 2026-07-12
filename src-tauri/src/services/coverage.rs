use crate::domain::{
    AppState, Chapter, NovelSettings, ParsedChapterRewrite, ReviewCoverageItem, ReviewDecision,
    ReviewIssue, RewritePlan, RewriteReviewDecision, RewriteStateUpdate,
};
use crate::{
    obligation_mode_rule, parse_jsonish_value, parse_review_decision_output, to_string,
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
    if evidence.is_empty() {
        return false;
    }
    rewrites.iter().any(|rewrite| {
        if !item.chapter_indexes.is_empty() && !item.chapter_indexes.contains(&rewrite.index) {
            return false;
        }
        let rewrite_text = format!("{}\n{}", rewrite.title, rewrite.text);
        let normalized_rewrite = normalize_coverage_evidence(&rewrite_text);
        let normalized_evidence = normalize_coverage_evidence(evidence);
        if !normalized_evidence.is_empty() && normalized_rewrite.contains(&normalized_evidence) {
            return true;
        }

        let quoted_fragments = extract_quoted_coverage_evidence(evidence);
        if !quoted_fragments.is_empty() {
            let required_matches = (quoted_fragments.len() * 2).div_ceil(3);
            let matched = quoted_fragments
                .iter()
                .filter(|fragment| coverage_fragment_exists(&normalized_rewrite, fragment))
                .count();
            if matched >= required_matches {
                return true;
            }
        }

        let fragments = split_coverage_evidence(evidence);
        if fragments.is_empty() {
            return false;
        }
        let required_matches = (fragments.len() * 2).div_ceil(3);
        let matched = fragments
            .iter()
            .filter(|fragment| coverage_fragment_exists(&normalized_rewrite, fragment))
            .count();
        matched >= required_matches
    })
}

fn coverage_fragment_exists(normalized_rewrite: &str, fragment: &str) -> bool {
    if normalized_rewrite.contains(fragment) {
        return true;
    }
    let needle = fragment.chars().collect::<Vec<_>>();
    if needle.len() < 8 {
        return false;
    }
    let haystack = normalized_rewrite.chars().collect::<Vec<_>>();
    let minimum = needle.len().saturating_sub(1);
    let maximum = needle.len() + 1;
    (minimum..=maximum).any(|window_length| {
        window_length > 0
            && window_length <= haystack.len()
            && haystack
                .windows(window_length)
                .any(|window| within_one_edit(window, &needle))
    })
}

fn within_one_edit(left: &[char], right: &[char]) -> bool {
    if left.len().abs_diff(right.len()) > 1 {
        return false;
    }
    if left.len() == right.len() {
        return left
            .iter()
            .zip(right)
            .filter(|(left, right)| left != right)
            .count()
            <= 1;
    }
    let (shorter, longer) = if left.len() < right.len() {
        (left, right)
    } else {
        (right, left)
    };
    let (mut short_index, mut long_index, mut skipped) = (0, 0, false);
    while short_index < shorter.len() && long_index < longer.len() {
        if shorter[short_index] == longer[long_index] {
            short_index += 1;
            long_index += 1;
        } else if skipped {
            return false;
        } else {
            skipped = true;
            long_index += 1;
        }
    }
    true
}

fn extract_quoted_coverage_evidence(evidence: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    for (open, close) in [('“', '”'), ('‘', '’'), ('"', '"'), ('\'', '\'')] {
        let mut start = None;
        for (index, character) in evidence.char_indices() {
            if character == open && start.is_none() {
                start = Some(index + character.len_utf8());
            } else if character == close {
                if let Some(start_index) = start.take() {
                    let normalized = normalize_coverage_evidence(&evidence[start_index..index]);
                    if normalized.chars().count() >= 3 {
                        fragments.push(normalized);
                    }
                } else if open == close {
                    start = Some(index + character.len_utf8());
                }
            }
        }
    }
    fragments.sort();
    fragments.dedup();
    fragments
}

fn normalize_coverage_evidence(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn split_coverage_evidence(evidence: &str) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut current = String::new();
    for character in evidence.chars() {
        if character.is_whitespace()
            || matches!(
                character,
                '。' | '！' | '？' | '!' | '?' | '；' | ';' | '…' | '⋯' | '.' | '．'
            )
        {
            let normalized = normalize_coverage_evidence(&current);
            if normalized.chars().count() >= 3 {
                fragments.push(normalized);
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    let normalized = normalize_coverage_evidence(&current);
    if normalized.chars().count() >= 3 {
        fragments.push(normalized);
    }
    fragments.sort();
    fragments.dedup();
    fragments
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
    // Continuity updates are contract data, not a model judgment. Requiring the reviewer to
    // reproduce a potentially long array caused false quality-gate failures from omitted or
    // slightly rewritten metadata. Once coverage passes, persist the canonical plan states.
    let state_updates = canonical_planned_state_updates(plan);

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
        let preserve_needs_no_change_evidence = obligation_mode_rule(&obligation.rule_ids)
            == Some("R3_PRESERVE")
            && obligation.required_changes.is_empty();
        let evidence_exists = preserve_needs_no_change_evidence
            || evidence_exists_in_rewrite(item, rewrites);
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
                    obligation_fix_summary(obligation)
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
                    obligation_fix_summary(obligation)
                ),
            });
        }
    }

    decision.approved = coverage_gate_passes(plan, &coverage, &decision.issues);
    Ok(RewriteReviewDecision {
        decision,
        coverage,
        state_updates,
    })
}

fn obligation_fix_summary(obligation: &crate::domain::RewriteObligation) -> String {
    if !obligation.required_changes.is_empty() {
        obligation.required_changes.join("；")
    } else if !obligation.preserve.is_empty() {
        format!("按原文恢复并保留：{}", obligation.preserve.join("；"))
    } else {
        "按原文章节恢复该节点，不新增或跨章搬运内容".to_string()
    }
}

fn canonical_planned_state_updates(plan: &RewritePlan) -> Vec<RewriteStateUpdate> {
    let states = if plan.planned_state_updates.is_empty() {
        plan.obligations
            .iter()
            .flat_map(|obligation| obligation.planned_state_updates.iter())
            .collect::<Vec<_>>()
    } else {
        plan.planned_state_updates.iter().collect::<Vec<_>>()
    };
    let mut positions = HashMap::<(String, String), usize>::new();
    let mut canonical = Vec::<RewriteStateUpdate>::new();
    for state in states {
        let key = (
            state.thread_key.trim().to_string(),
            state.state_type.trim().to_string(),
        );
        if let Some(position) = positions.get(&key).copied() {
            if state.chapter_index > canonical[position].chapter_index {
                canonical[position] = state.clone();
            }
        } else {
            positions.insert(key, canonical.len());
            canonical.push(state.clone());
        }
    }
    canonical.sort_by_key(|state| {
        (
            state.chapter_index,
            state.thread_key.clone(),
            state.state_type.clone(),
        )
    });
    canonical
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
        let compatible = crate::services::contracts::load_compatible_continuity_json(
            &tx,
            novel_id,
        )?;
        let existing = serde_json::from_str::<Vec<RewriteStateUpdate>>(&compatible)
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

    fn settings() -> NovelSettings {
        NovelSettings {
            novel_id: "novel-1".to_string(),
            protagonist_name: "许纸".to_string(),
            protagonist_aliases: String::new(),
            rewritten_protagonist_name: "白纸".to_string(),
            additional_feminize_names: String::new(),
            bust: "普通".to_string(),
            body_type: "少女".to_string(),
            rewrite_mode: "strict".to_string(),
            advanced_settings: String::new(),
            relationship_targets: "[]".to_string(),
            updated_at: "now".to_string(),
        }
    }

    #[test]
    fn coverage_evidence_accepts_a_real_fragment_majority_but_not_half_fabrication() {
        let rewrites = vec![ParsedChapterRewrite {
            id: "chapter-3".to_string(),
            index: 3,
            title: "第三章".to_string(),
            text: "她手掌轻轻拢了拢稀少的头发。中间还有一段叙述。大叔看着这姑娘病恹恹的样子，也不好意思靠太近。".to_string(),
        }];
        let mut item = ReviewCoverageItem {
            obligation_id: "O-1".to_string(),
            status: "satisfied".to_string(),
            chapter_indexes: vec![3],
            evidence: "她手掌轻轻拢了拢稀少的头发 大叔看着这姑娘病恹恹的样子，也不好意思靠太近!".to_string(),
        };

        assert!(evidence_exists_in_rewrite(&item, &rewrites));

        item.evidence.push_str("；管理员主动送给她一份礼物");
        assert!(evidence_exists_in_rewrite(&item, &rewrites));

        item.evidence.push_str("；管理员又主动替她安排了住处");
        assert!(!evidence_exists_in_rewrite(&item, &rewrites));
    }

    #[test]
    fn coverage_evidence_accepts_two_thirds_exact_quotes_and_rejects_a_weak_match() {
        let rewrites = vec![ParsedChapterRewrite {
            id: "chapter-5".to_string(),
            index: 5,
            title: "第五章".to_string(),
            text: "她说：‘她是女神！’随后俯下身，长发如瀑布般垂落。".to_string(),
        }];
        let mut item = ReviewCoverageItem {
            obligation_id: "O-1".to_string(),
            status: "satisfied".to_string(),
            chapter_indexes: vec![5],
            evidence: "虫猿：‘她是女神！’；旁白：‘俯下身，长发如瀑布般垂落’；概括：‘不存在的第三段证据’"
                .to_string(),
        };

        assert!(evidence_exists_in_rewrite(&item, &rewrites));

        item.evidence = "虫猿：‘她是女神！’；概括：‘不存在的第二段证据’；概括：‘不存在的第三段证据’"
            .to_string();
        assert!(!evidence_exists_in_rewrite(&item, &rewrites));
    }

    #[test]
    fn coverage_evidence_splits_unquoted_ellipsis_separated_excerpts() {
        let rewrites = vec![ParsedChapterRewrite {
            id: "chapter-3".to_string(),
            index: 3,
            title: "第三章".to_string(),
            text: "大叔眼里涌出不加掩饰的心疼和怜悯。姑娘，你身体不好可别省着。她笑得像是个脸色苍白却带着狡黠的少女。".to_string(),
        }];
        let item = ReviewCoverageItem {
            obligation_id: "O-1".to_string(),
            status: "satisfied".to_string(),
            chapter_indexes: vec![3],
            evidence: "眼里涌出不加掩饰的心疼和怜悯……姑娘，你身体不好可别省着……笑得像是个脸色苍白却带着狡黠的少女".to_string(),
        };

        assert!(evidence_exists_in_rewrite(&item, &rewrites));
    }

    #[test]
    fn coverage_evidence_tolerates_one_character_transcription_drift() {
        let rewrites = vec![ParsedChapterRewrite {
            id: "chapter-1".to_string(),
            index: 1,
            title: "第一章".to_string(),
            text: "她吃力地拖着行李箱一路上气不接下气，细瘦的手臂几乎拽不动轮子。用清秀的字迹记录进程。".to_string(),
        }];
        let item = ReviewCoverageItem {
            obligation_id: "O-1".to_string(),
            status: "satisfied".to_string(),
            chapter_indexes: vec![1],
            evidence: "吃力地拖着行李箱一路上气不接上气，细瘦的手臂几乎拽不动轮子；她用清秀的字迹记录进程".to_string(),
        };

        assert!(evidence_exists_in_rewrite(&item, &rewrites));
    }

    #[test]
    fn coverage_evidence_counts_specific_three_character_terms() {
        let rewrites = vec![ParsedChapterRewrite {
            id: "chapter-3".to_string(),
            index: 3,
            title: "第三章".to_string(),
            text: "白纸发现海生动物已经登陆，又用清秀的字迹记录下光武纪的后面一页，并命名为新生纪。".to_string(),
        }];
        let item = ReviewCoverageItem {
            obligation_id: "O-1".to_string(),
            status: "satisfied".to_string(),
            chapter_indexes: vec![3],
            evidence: "白纸观察到；又用清秀的字迹记录下光武纪的后面一页；新生纪"
                .to_string(),
        };

        assert!(evidence_exists_in_rewrite(&item, &rewrites));
    }

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
    fn reviewer_state_echo_is_ignored_and_plan_states_are_canonicalized() {
        let first = RewriteStateUpdate {
            thread_key: "基因获取线".to_string(),
            state_type: "获得".to_string(),
            value: "白纸购得猩猩血液并约定长期合作".to_string(),
            chapter_index: 3,
            source_obligation_ids: vec!["O-1".to_string()],
        };
        let duplicate = RewriteStateUpdate {
            value: "白纸购得猩猩血液".to_string(),
            source_obligation_ids: vec!["O-2".to_string()],
            ..first.clone()
        };
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v2".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![
                RewriteObligation {
                    obligation_id: "O-1".to_string(),
                    node_id: "N-1".to_string(),
                    chapter_index: 3,
                    rule_ids: Vec::new(),
                    preserve: Vec::new(),
                    required_changes: Vec::new(),
                    deep_delta_categories: Vec::new(),
                    forbidden_regressions: Vec::new(),
                    downstream_effects: Vec::new(),
                    planned_state_updates: vec![first.clone()],
                },
                RewriteObligation {
                    obligation_id: "O-2".to_string(),
                    node_id: "N-2".to_string(),
                    chapter_index: 3,
                    rule_ids: Vec::new(),
                    preserve: Vec::new(),
                    required_changes: Vec::new(),
                    deep_delta_categories: Vec::new(),
                    forbidden_regressions: Vec::new(),
                    downstream_effects: Vec::new(),
                    planned_state_updates: vec![duplicate],
                },
            ],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };

        assert_eq!(canonical_planned_state_updates(&plan), vec![first]);
    }

    #[test]
    fn review_can_pass_without_echoing_contract_state_metadata() {
        let state = RewriteStateUpdate {
            thread_key: "人际关系线".to_string(),
            state_type: "边界".to_string(),
            value: "白纸与陈熙保持普通朋友关系".to_string(),
            chapter_index: 3,
            source_obligation_ids: vec!["O-1".to_string()],
        };
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v2".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![RewriteObligation {
                obligation_id: "O-1".to_string(),
                node_id: "N-1".to_string(),
                chapter_index: 3,
                rule_ids: vec!["R3_PRESERVE".to_string()],
                preserve: vec!["保留普通朋友关系".to_string()],
                required_changes: Vec::new(),
                deep_delta_categories: Vec::new(),
                forbidden_regressions: Vec::new(),
                downstream_effects: Vec::new(),
                planned_state_updates: vec![state.clone()],
            }],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };
        let rewrites = vec![ParsedChapterRewrite {
            id: "chapter-3".to_string(),
            index: 3,
            title: "第三章".to_string(),
            text: "白纸与陈熙仍是普通朋友。".to_string(),
        }];
        let output = r#"{
          "approved": true,
          "coverage": [{"obligation_id":"O-1","status":"satisfied","chapter_indexes":[3],"evidence":"模型误引了不存在的后续对话"}],
          "issues": []
        }"#;

        let parsed =
            parse_rewrite_review_decision_output(output, &settings(), &plan, &rewrites).unwrap();

        assert!(parsed.decision.approved);
        assert_eq!(parsed.state_updates, vec![state]);
    }

    #[test]
    fn preserve_regression_still_blocks_and_gets_an_actionable_fix() {
        let obligation = RewriteObligation {
            obligation_id: "O-1".to_string(),
            node_id: "N-1".to_string(),
            chapter_index: 5,
            rule_ids: vec!["R3_PRESERVE".to_string()],
            preserve: vec!["保留第五章结尾，不得提前搬入第六章对话".to_string()],
            required_changes: Vec::new(),
            deep_delta_categories: Vec::new(),
            forbidden_regressions: Vec::new(),
            downstream_effects: Vec::new(),
            planned_state_updates: Vec::new(),
        };

        assert_eq!(
            obligation_fix_summary(&obligation),
            "按原文恢复并保留：保留第五章结尾，不得提前搬入第六章对话"
        );
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v2.2".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![obligation],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };
        let rewrites = vec![ParsedChapterRewrite {
            id: "chapter-5".to_string(),
            index: 5,
            title: "第五章".to_string(),
            text: "正文仍在，但被提前搬入了后章对话。".to_string(),
        }];
        let output = r#"{
          "approved": false,
          "coverage": [{"obligation_id":"O-1","status":"regressed","chapter_indexes":[5],"evidence":"正文仍在"}],
          "issues": []
        }"#;

        let parsed =
            parse_rewrite_review_decision_output(output, &settings(), &plan, &rewrites).unwrap();
        assert!(!parsed.decision.approved);
        assert!(parsed
            .decision
            .issues
            .iter()
            .any(|issue| issue.required_fix.contains("按原文恢复并保留")));
    }

    #[test]
    fn top_level_planned_states_supersede_obligation_intermediate_states() {
        let early = RewriteStateUpdate {
            thread_key: "陈熙友谊线".to_string(),
            state_type: "互动模式".to_string(),
            value: "陈熙开始像保护闺蜜一样维护主角".to_string(),
            chapter_index: 5,
            source_obligation_ids: vec!["O-5".to_string()],
        };
        let final_state = RewriteStateUpdate {
            thread_key: "陈熙友谊线".to_string(),
            state_type: "互动模式".to_string(),
            value: "女性闺蜜间的调侃、照顾和亲密".to_string(),
            chapter_index: 7,
            source_obligation_ids: vec!["O-5".to_string(), "O-7".to_string()],
        };
        let obligation = |id: &str, chapter_index: i64, states: Vec<RewriteStateUpdate>| {
            RewriteObligation {
                obligation_id: id.to_string(),
                node_id: format!("N-{chapter_index}"),
                chapter_index,
                rule_ids: Vec::new(),
                preserve: Vec::new(),
                required_changes: Vec::new(),
                deep_delta_categories: Vec::new(),
                forbidden_regressions: Vec::new(),
                downstream_effects: Vec::new(),
                planned_state_updates: states,
            }
        };
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![
                obligation("O-5", 5, vec![early]),
                obligation("O-7", 7, vec![final_state.clone()]),
            ],
            planned_state_updates: vec![final_state.clone()],
            cross_shard_dependencies: Vec::new(),
        };

        assert_eq!(canonical_planned_state_updates(&plan), vec![final_state]);
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
