use crate::domain::{RewritePlan, RewriteStateUpdate, SourceImpactNode};
use crate::{IMPACT_GRAPH_ASSET_KIND, REWRITE_CONTINUITY_ASSET_KIND};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const CONTINUITY_FALLBACK_LIMIT: usize = 8;

fn compact_json(value: &Value, fallback: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| fallback.to_string())
}

fn state_value(state: &RewriteStateUpdate) -> Value {
    json!({
        "thread_key": state.thread_key,
        "state_type": state.state_type,
        "value": state.value,
        "chapter_index": state.chapter_index,
        "source_obligation_ids": state.source_obligation_ids,
    })
}

pub(crate) fn is_managed_graph_asset_kind(kind: &str) -> bool {
    matches!(
        kind,
        IMPACT_GRAPH_ASSET_KIND | REWRITE_CONTINUITY_ASSET_KIND
    )
}

pub(crate) fn format_planning_nodes(nodes: &[SourceImpactNode]) -> String {
    let values = nodes
        .iter()
        .map(|node| {
            json!({
                "node_id": node.node_id,
                "chapter_index": node.chapter_index,
                "presence_kind": node.presence_kind,
                "participants": node.participants,
                "source_evidence": node.source_evidence,
                "thread_keys": node.thread_keys,
                "links": node.links,
            })
        })
        .collect::<Vec<_>>();
    compact_json(&Value::Array(values), "[]")
}

pub(crate) fn format_execution_nodes(nodes: &[SourceImpactNode]) -> String {
    let values = nodes
        .iter()
        .map(|node| {
            json!({
                "node_id": node.node_id,
                "chapter_index": node.chapter_index,
                "presence_kind": node.presence_kind,
                "participants": node.participants,
                "source_evidence": node.source_evidence,
                "thread_keys": node.thread_keys,
            })
        })
        .collect::<Vec<_>>();
    compact_json(&Value::Array(values), "[]")
}

pub(crate) fn format_prior_contract_context(
    plans: &[RewritePlan],
    current_nodes: &[SourceImpactNode],
    dependency_graph: &[SourceImpactNode],
) -> String {
    let current_node_ids = current_nodes
        .iter()
        .map(|node| node.node_id.trim())
        .filter(|node_id| !node_id.is_empty())
        .collect::<HashSet<_>>();
    let mut linked_node_ids = current_nodes
        .iter()
        .flat_map(|node| node.links.iter())
        .map(|link| link.target.trim())
        .filter(|target| !target.is_empty())
        .collect::<HashSet<_>>();
    linked_node_ids.extend(
        dependency_graph
            .iter()
            .filter(|node| {
                node.links
                    .iter()
                    .any(|link| current_node_ids.contains(link.target.trim()))
            })
            .map(|node| node.node_id.trim())
            .filter(|node_id| !node_id.is_empty()),
    );
    let thread_keys = current_nodes
        .iter()
        .flat_map(|node| node.thread_keys.iter())
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .collect::<HashSet<_>>();
    let is_relevant = |obligation: &&crate::domain::RewriteObligation| {
        linked_node_ids.contains(obligation.node_id.trim())
            || obligation.planned_state_updates.iter().any(|state| {
                thread_keys.contains(state.thread_key.trim())
                    || is_global_thread_key(&state.thread_key)
            })
    };
    let mut selected = plans
        .iter()
        .flat_map(|plan| plan.obligations.iter())
        .filter(is_relevant)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        selected.extend(
            plans
                .last()
                .into_iter()
                .flat_map(|plan| plan.obligations.iter()),
        );
    }
    let values = selected
        .into_iter()
        .map(|obligation| {
            json!({
                "obligation_id": obligation.obligation_id,
                "node_id": obligation.node_id,
                "chapter_index": obligation.chapter_index,
                "rule_ids": obligation.rule_ids,
                "downstream_effects": obligation.downstream_effects,
            })
        })
        .collect::<Vec<_>>();
    compact_json(&Value::Array(values), "[]")
}

pub(crate) fn format_execution_contract(
    plan: &RewritePlan,
    chapter_indexes: Option<&HashSet<i64>>,
) -> String {
    let include_chapter =
        |chapter_index: i64| chapter_indexes.is_none_or(|indexes| indexes.contains(&chapter_index));
    let obligations = plan
        .obligations
        .iter()
        .filter(|obligation| include_chapter(obligation.chapter_index))
        .collect::<Vec<_>>();
    let planned_state_updates = plan
        .planned_state_updates
        .iter()
        .filter(|state| state.chapter_index == 0 || include_chapter(state.chapter_index))
        .collect::<Vec<_>>();
    compact_json(
        &json!({
            "plan_version": plan.plan_version,
            "obligations": obligations,
            "planned_state_updates": planned_state_updates,
        }),
        "{}",
    )
}

pub(crate) fn format_repair_contract(
    plan: &RewritePlan,
    chapter_indexes: Option<&HashSet<i64>>,
    failed_obligation_ids: &HashSet<String>,
) -> String {
    let include_chapter =
        |chapter_index: i64| chapter_indexes.is_none_or(|indexes| indexes.contains(&chapter_index));
    let obligations = plan
        .obligations
        .iter()
        .filter(|obligation| include_chapter(obligation.chapter_index))
        .collect::<Vec<_>>();
    let obligation_ids = obligations
        .iter()
        .map(|obligation| obligation.obligation_id.as_str())
        .collect::<HashSet<_>>();
    let mut repair_target_obligation_ids = failed_obligation_ids
        .iter()
        .filter(|id| obligation_ids.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    repair_target_obligation_ids.sort();
    let planned_state_updates = plan
        .planned_state_updates
        .iter()
        .filter(|state| state.chapter_index == 0 || include_chapter(state.chapter_index))
        .collect::<Vec<_>>();
    compact_json(
        &json!({
            "plan_version": plan.plan_version,
            "repair_target_obligation_ids": repair_target_obligation_ids,
            "obligations": obligations,
            "planned_state_updates": planned_state_updates,
        }),
        "{}",
    )
}

fn relevant_thread_keys(nodes: &[SourceImpactNode], plan: Option<&RewritePlan>) -> HashSet<String> {
    let mut keys = nodes
        .iter()
        .flat_map(|node| node.thread_keys.iter())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .collect::<HashSet<_>>();
    if let Some(plan) = plan {
        for state in plan.planned_state_updates.iter().chain(
            plan.obligations
                .iter()
                .flat_map(|obligation| obligation.planned_state_updates.iter()),
        ) {
            let key = state.thread_key.trim();
            if !key.is_empty() {
                keys.insert(key.to_string());
            }
        }
    }
    keys
}

fn is_global_thread_key(key: &str) -> bool {
    matches!(
        key.trim().to_ascii_lowercase().as_str(),
        "global" | "protagonist"
    ) || matches!(key.trim(), "全局" | "主角")
}

pub(crate) fn project_relevant_continuity(
    stored_json: &str,
    nodes: &[SourceImpactNode],
    plan: Option<&RewritePlan>,
    accumulated_state: &[RewriteStateUpdate],
) -> String {
    let mut states =
        serde_json::from_str::<Vec<RewriteStateUpdate>>(stored_json).unwrap_or_default();
    states.extend_from_slice(accumulated_state);

    let mut latest = HashMap::<(String, String), RewriteStateUpdate>::new();
    for state in states {
        let key = (state.thread_key.clone(), state.state_type.clone());
        if latest
            .get(&key)
            .is_none_or(|current| state.chapter_index >= current.chapter_index)
        {
            latest.insert(key, state);
        }
    }

    let keys = relevant_thread_keys(nodes, plan);
    let mut selected = latest
        .into_values()
        .filter(|state| {
            keys.is_empty()
                || keys.contains(state.thread_key.trim())
                || is_global_thread_key(&state.thread_key)
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|state| {
        (
            state.chapter_index,
            state.thread_key.clone(),
            state.state_type.clone(),
        )
    });
    if keys.is_empty() && selected.len() > CONTINUITY_FALLBACK_LIMIT {
        selected.drain(..selected.len() - CONTINUITY_FALLBACK_LIMIT);
    }
    compact_json(
        &Value::Array(selected.iter().map(state_value).collect()),
        "[]",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CanonAsset, RewriteObligation, SourceImpactLink};

    fn node(thread_key: &str) -> SourceImpactNode {
        SourceImpactNode {
            node_id: "N-1".to_string(),
            chapter_id: "chapter-1".to_string(),
            chapter_index: 1,
            ordinal: 1,
            presence_kind: "direct".to_string(),
            participants: vec!["主角".to_string()],
            source_evidence: "很长的原文短证据".to_string(),
            narrative_function: "推动事件".to_string(),
            gender_mechanisms: vec!["旁人判断".to_string()],
            state_before: "陌生".to_string(),
            state_after: "认识".to_string(),
            thread_keys: vec![thread_key.to_string()],
            links: vec![SourceImpactLink {
                kind: "continues".to_string(),
                target: "N-2".to_string(),
            }],
            confidence: 0.9,
        }
    }

    fn state(thread_key: &str, value: &str, chapter_index: i64) -> RewriteStateUpdate {
        RewriteStateUpdate {
            thread_key: thread_key.to_string(),
            state_type: "关系".to_string(),
            value: value.to_string(),
            chapter_index,
            source_obligation_ids: vec![format!("O-{chapter_index}")],
        }
    }

    #[test]
    fn execution_nodes_keep_local_edit_anchor_but_omit_graph_only_fields() {
        let nodes = vec![node("关系线-A")];
        let planning = format_planning_nodes(&nodes);
        let execution = format_execution_nodes(&nodes);

        assert!(planning.contains("source_evidence"));
        assert!(execution.contains("source_evidence"));
        assert!(!planning.contains("narrative_function"));
        assert!(!execution.contains("state_after"));
        assert!(!execution.contains("confidence"));
        assert!(!execution.contains("links"));
        assert!(execution.len() < planning.len());
    }

    #[test]
    fn continuity_projection_keeps_latest_relevant_state() {
        let stored = serde_json::to_string(&vec![
            state("关系线-A", "旧", 1),
            state("关系线-A", "新", 3),
            state("关系线-B", "无关", 4),
        ])
        .unwrap();
        let projected = project_relevant_continuity(
            &stored,
            &[node("关系线-A")],
            None,
            &[state("关系线-A", "计划更新", 5)],
        );

        assert!(projected.contains("计划更新"));
        assert!(!projected.contains("无关"));
        assert!(!projected.contains("\"旧\""));
    }

    #[test]
    fn prior_contract_projection_omits_repeated_rewrite_details() {
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: vec![node("关系线-A")],
            obligations: vec![RewriteObligation {
                obligation_id: "O-N-1".to_string(),
                node_id: "N-1".to_string(),
                chapter_index: 1,
                rule_ids: vec!["R3_PROTAGONIST_NODE_DELTA".to_string()],
                preserve: vec!["保留事件".to_string()],
                required_changes: vec!["改变互动边界".to_string()],
                deep_delta_categories: vec!["interaction_boundary".to_string()],
                forbidden_regressions: vec!["不得回退".to_string()],
                downstream_effects: vec!["后续关系承接".to_string()],
                planned_state_updates: vec![state("关系线-A", "认识", 1)],
            }],
            planned_state_updates: vec![state("关系线-A", "认识", 1)],
            cross_shard_dependencies: Vec::new(),
        };
        let compact =
            format_prior_contract_context(std::slice::from_ref(&plan), &[node("关系线-A")], &[]);
        let full = serde_json::to_string_pretty(&vec![plan]).unwrap();

        assert!(compact.contains("obligation_id"));
        assert!(compact.contains("downstream_effects"));
        assert!(!compact.contains("required_changes"));
        assert!(compact.len() * 2 < full.len());
    }

    #[test]
    fn prior_contract_projection_follows_incoming_forward_edge() {
        let make_plan = |obligation_id: &str, node_id: &str, chapter_index: i64| RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![RewriteObligation {
                obligation_id: obligation_id.to_string(),
                node_id: node_id.to_string(),
                chapter_index,
                rule_ids: vec!["R3_PROTAGONIST_NODE_DELTA".to_string()],
                preserve: Vec::new(),
                required_changes: vec!["改变互动边界".to_string()],
                deep_delta_categories: vec!["interaction_boundary".to_string()],
                forbidden_regressions: Vec::new(),
                downstream_effects: vec!["后续承接".to_string()],
                planned_state_updates: Vec::new(),
            }],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };
        let mut predecessor = node("前序线");
        predecessor.node_id = "prior-node".to_string();
        predecessor.links = vec![SourceImpactLink {
            kind: "continues".to_string(),
            target: "current-node".to_string(),
        }];
        let mut current = node("当前线");
        current.node_id = "current-node".to_string();
        current.links.clear();
        let plans = vec![
            make_plan("O-prior", "prior-node", 1),
            make_plan("O-unrelated", "unrelated-node", 2),
        ];

        let compact = format_prior_contract_context(&plans, &[current], &[predecessor]);

        assert!(compact.contains("O-prior"));
        assert!(!compact.contains("O-unrelated"));
    }

    #[test]
    fn generic_canon_context_excludes_managed_graph_assets() {
        let assets = vec![
            CanonAsset {
                novel_id: "novel-1".to_string(),
                kind: "人物卡".to_string(),
                content: "主角人物事实".to_string(),
                updated_at: String::new(),
            },
            CanonAsset {
                novel_id: "novel-1".to_string(),
                kind: IMPACT_GRAPH_ASSET_KIND.to_string(),
                content: "不应重复进入通用上下文".to_string(),
                updated_at: String::new(),
            },
            CanonAsset {
                novel_id: "novel-1".to_string(),
                kind: REWRITE_CONTINUITY_ASSET_KIND.to_string(),
                content: "不应重复进入通用上下文".to_string(),
                updated_at: String::new(),
            },
        ];

        let compact = crate::build_compact_canon_text(&assets);

        assert!(compact.contains("人物卡"));
        assert!(!compact.contains(IMPACT_GRAPH_ASSET_KIND));
        assert!(!compact.contains(REWRITE_CONTINUITY_ASSET_KIND));
        assert!(!compact.contains("不应重复进入通用上下文"));
    }

    #[test]
    fn repair_contract_keeps_all_target_chapter_obligations_and_marks_failed_ones() {
        let make_obligation = |id: &str, chapter_index: i64| RewriteObligation {
            obligation_id: id.to_string(),
            node_id: format!("N-{id}"),
            chapter_index,
            rule_ids: Vec::new(),
            preserve: Vec::new(),
            required_changes: vec![format!("修复-{id}")],
            deep_delta_categories: Vec::new(),
            forbidden_regressions: Vec::new(),
            downstream_effects: Vec::new(),
            planned_state_updates: Vec::new(),
        };
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![
                make_obligation("O-1", 1),
                make_obligation("O-2", 1),
                make_obligation("O-3", 2),
            ],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };
        let chapters = HashSet::from([1]);
        let failed = HashSet::from(["O-2".to_string()]);

        let contract = format_repair_contract(&plan, Some(&chapters), &failed);

        assert!(contract.contains("O-1"));
        assert!(contract.contains("O-2"));
        assert!(!contract.contains("O-3"));
        assert!(contract.contains("\"repair_target_obligation_ids\":[\"O-2\"]"));
    }
}
