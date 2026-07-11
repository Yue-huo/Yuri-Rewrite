use crate::domain::{RewritePlan, SourceImpactNode};
use std::collections::{BTreeSet, HashMap, HashSet};

fn dependency_token(value: &str) -> &str {
    let value = value.trim();
    for prefix in [
        "node:",
        "obligation:",
        "thread:",
        "node_id:",
        "obligation_id:",
        "thread_key:",
    ] {
        if let Some(value) = value.strip_prefix(prefix) {
            return value.trim();
        }
    }
    value
}

fn is_thread_dependency(value: &str) -> bool {
    let value = value.trim();
    value.starts_with("thread:") || value.starts_with("thread_key:")
}

fn normalized_thread_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn fuzzy_prior_thread_owners(
    dependency: &str,
    shard_index: usize,
    thread_owners: &HashMap<String, BTreeSet<usize>>,
) -> BTreeSet<usize> {
    let dependency = normalized_thread_key(dependency);
    let minimum_length = if dependency.is_ascii() { 4 } else { 2 };
    if dependency.chars().count() < minimum_length {
        return BTreeSet::new();
    }
    thread_owners
        .iter()
        .filter(|(thread, _)| {
            let thread = normalized_thread_key(thread);
            thread.contains(&dependency) || dependency.contains(&thread)
        })
        .flat_map(|(_, owners)| owners.range(..shard_index).copied())
        .collect()
}

fn add_edge(edges: &mut [HashSet<usize>], from: usize, to: usize) -> Result<(), String> {
    if from == to {
        return Err(format!("分片 {} 声明了指向自身的跨分片依赖。", to + 1));
    }
    edges[from].insert(to);
    Ok(())
}

pub(crate) fn build_dependency_levels(
    plans: &[RewritePlan],
    graph: &[SourceImpactNode],
) -> Result<Vec<usize>, String> {
    if plans.is_empty() {
        return Ok(Vec::new());
    }

    let mut node_owner = HashMap::<String, usize>::new();
    let mut obligation_owner = HashMap::<String, usize>::new();
    let mut thread_owners = HashMap::<String, BTreeSet<usize>>::new();
    for (shard_index, plan) in plans.iter().enumerate() {
        for obligation in &plan.obligations {
            if node_owner
                .insert(obligation.node_id.clone(), shard_index)
                .is_some()
            {
                return Err(format!(
                    "节点 {} 同时出现在多个分片契约中。",
                    obligation.node_id
                ));
            }
            if obligation_owner
                .insert(obligation.obligation_id.clone(), shard_index)
                .is_some()
            {
                return Err(format!(
                    "义务 {} 同时出现在多个分片契约中。",
                    obligation.obligation_id
                ));
            }
            for state in &obligation.planned_state_updates {
                if !state.thread_key.trim().is_empty() {
                    thread_owners
                        .entry(state.thread_key.trim().to_string())
                        .or_default()
                        .insert(shard_index);
                }
            }
        }
        for state in &plan.planned_state_updates {
            if !state.thread_key.trim().is_empty() {
                thread_owners
                    .entry(state.thread_key.trim().to_string())
                    .or_default()
                    .insert(shard_index);
            }
        }
    }
    for node in graph {
        let Some(&owner) = node_owner.get(&node.node_id) else {
            continue;
        };
        for thread_key in &node.thread_keys {
            if !thread_key.trim().is_empty() {
                thread_owners
                    .entry(thread_key.trim().to_string())
                    .or_default()
                    .insert(owner);
            }
        }
    }

    let mut edges = vec![HashSet::<usize>::new(); plans.len()];
    for node in graph {
        let Some(&source_owner) = node_owner.get(&node.node_id) else {
            continue;
        };
        for link in &node.links {
            let Some(&target_owner) = node_owner.get(&link.target) else {
                continue;
            };
            if source_owner != target_owner {
                add_edge(&mut edges, source_owner, target_owner)?;
            }
        }
    }

    for (shard_index, plan) in plans.iter().enumerate() {
        for raw_dependency in &plan.cross_shard_dependencies {
            let dependency = dependency_token(raw_dependency);
            let thread_dependency = is_thread_dependency(raw_dependency);
            if dependency.is_empty() {
                return Err(format!("分片 {} 包含空的跨分片依赖。", shard_index + 1));
            }
            let references_own_node_or_obligation = node_owner
                .get(dependency)
                .or_else(|| obligation_owner.get(dependency))
                .is_some_and(|owner| *owner == shard_index);
            let references_current_thread_without_a_prior_owner = thread_owners
                .get(dependency)
                .is_some_and(|owners| {
                    owners.contains(&shard_index)
                        && owners.range(..shard_index).next_back().is_none()
                });
            if references_own_node_or_obligation || references_current_thread_without_a_prior_owner
            {
                // Models occasionally repeat an ID or thread created by the current shard in the
                // cross-shard field. It carries no scheduling information and must not make an
                // otherwise valid plan fail as an unresolvable dependency.
                continue;
            }
            let owner = node_owner
                .get(dependency)
                .or_else(|| obligation_owner.get(dependency))
                .copied()
                .or_else(|| {
                    thread_owners.get(dependency).and_then(|owners| {
                        owners
                            .range(..shard_index)
                            .next_back()
                            .copied()
                            .or_else(|| owners.iter().copied().find(|owner| *owner != shard_index))
                    })
                });
            if let Some(owner) = owner {
                add_edge(&mut edges, owner, shard_index)?;
                continue;
            }
            if thread_dependency {
                let fuzzy_owners = fuzzy_prior_thread_owners(
                    dependency,
                    shard_index,
                    &thread_owners,
                );
                if fuzzy_owners.is_empty() {
                    // A model may abbreviate a prior thread despite being told to copy its stable
                    // key. Waiting for every earlier shard is conservative: it preserves ordering
                    // without pretending the abbreviation identifies a specific relationship.
                    for owner in 0..shard_index {
                        add_edge(&mut edges, owner, shard_index)?;
                    }
                } else {
                    for owner in fuzzy_owners {
                        add_edge(&mut edges, owner, shard_index)?;
                    }
                }
                continue;
            }
            return Err(format!(
                "分片 {} 引用了无法解析的跨分片依赖：{}",
                shard_index + 1,
                raw_dependency
            ));
        }
    }

    let mut indegree = vec![0usize; plans.len()];
    for children in &edges {
        for child in children {
            indegree[*child] += 1;
        }
    }
    let mut ready = indegree
        .iter()
        .enumerate()
        .filter_map(|(index, degree)| (*degree == 0).then_some(index))
        .collect::<BTreeSet<_>>();
    let mut levels = vec![0usize; plans.len()];
    let mut visited = 0usize;
    while let Some(node) = ready.pop_first() {
        visited += 1;
        let mut children = edges[node].iter().copied().collect::<Vec<_>>();
        children.sort_unstable();
        for child in children {
            levels[child] = levels[child].max(levels[node] + 1);
            indegree[child] -= 1;
            if indegree[child] == 0 {
                ready.insert(child);
            }
        }
    }
    if visited != plans.len() {
        let cyclic = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, degree)| (*degree > 0).then_some((index + 1).to_string()))
            .collect::<Vec<_>>()
            .join("、");
        return Err(format!("跨分片依赖存在循环，涉及分片：{cyclic}"));
    }
    Ok(levels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RewriteObligation, SourceImpactLink};

    fn plan(index: usize, dependency: &[&str], thread_key: &str) -> RewritePlan {
        RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![RewriteObligation {
                obligation_id: format!("O-{index}"),
                node_id: format!("N-{index}"),
                chapter_index: index as i64 + 1,
                rule_ids: Vec::new(),
                preserve: Vec::new(),
                required_changes: Vec::new(),
                deep_delta_categories: Vec::new(),
                forbidden_regressions: Vec::new(),
                downstream_effects: Vec::new(),
                planned_state_updates: if thread_key.is_empty() {
                    Vec::new()
                } else {
                    vec![crate::domain::RewriteStateUpdate {
                        thread_key: thread_key.to_string(),
                        state_type: "关系".to_string(),
                        value: format!("状态-{index}"),
                        chapter_index: index as i64 + 1,
                        source_obligation_ids: vec![format!("O-{index}")],
                    }]
                },
            }],
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: dependency.iter().map(|value| value.to_string()).collect(),
        }
    }

    fn node(index: usize, target: Option<usize>) -> SourceImpactNode {
        SourceImpactNode {
            node_id: format!("N-{index}"),
            chapter_id: format!("chapter-{index}"),
            chapter_index: index as i64 + 1,
            ordinal: 1,
            presence_kind: "direct".to_string(),
            participants: Vec::new(),
            source_evidence: "证据".to_string(),
            narrative_function: "功能".to_string(),
            gender_mechanisms: Vec::new(),
            state_before: String::new(),
            state_after: String::new(),
            thread_keys: Vec::new(),
            links: target
                .map(|target| SourceImpactLink {
                    kind: "continues".to_string(),
                    target: format!("N-{target}"),
                })
                .into_iter()
                .collect(),
            confidence: 1.0,
        }
    }

    #[test]
    fn independent_shards_share_the_same_wave() {
        let plans = vec![plan(0, &[], ""), plan(1, &[], "")];
        assert_eq!(build_dependency_levels(&plans, &[]).unwrap(), vec![0, 0]);
    }

    #[test]
    fn graph_and_declared_dependencies_build_topological_levels() {
        let plans = vec![
            plan(0, &[], "承诺线"),
            plan(1, &["O-0"], ""),
            plan(2, &["thread:承诺线"], ""),
        ];
        let graph = vec![node(0, Some(1)), node(1, None), node(2, None)];
        assert_eq!(
            build_dependency_levels(&plans, &graph).unwrap(),
            vec![0, 1, 1]
        );
    }

    #[test]
    fn current_shard_ids_and_threads_are_ignored_as_non_cross_dependencies() {
        let plans = vec![
            plan(0, &[], ""),
            plan(1, &["O-1", "thread:当前关系线"], "当前关系线"),
        ];

        assert_eq!(build_dependency_levels(&plans, &[]).unwrap(), vec![0, 0]);
    }

    #[test]
    fn abbreviated_thread_dependency_matches_a_prior_stable_thread() {
        let plans = vec![
            plan(0, &[], "许纸与吉尔伽美什的师徒/神人关系线"),
            plan(1, &[], ""),
            plan(2, &["thread:师徒"], "主角-吉尔伽美什关系线"),
        ];

        assert_eq!(build_dependency_levels(&plans, &[]).unwrap(), vec![0, 0, 1]);
    }

    #[test]
    fn unknown_thread_dependency_conservatively_waits_for_all_prior_shards() {
        let plans = vec![
            plan(0, &[], ""),
            plan(1, &[], ""),
            plan(2, &["thread:模型自行概括的关系"], ""),
        ];

        assert_eq!(build_dependency_levels(&plans, &[]).unwrap(), vec![0, 0, 1]);
    }

    #[test]
    fn unknown_and_cyclic_dependencies_are_rejected() {
        let unknown = vec![plan(0, &[], ""), plan(1, &["missing"], "")];
        assert!(build_dependency_levels(&unknown, &[])
            .unwrap_err()
            .contains("无法解析"));

        let cyclic = vec![plan(0, &["O-1"], ""), plan(1, &["O-0"], "")];
        assert!(build_dependency_levels(&cyclic, &[])
            .unwrap_err()
            .contains("循环"));
    }
}
