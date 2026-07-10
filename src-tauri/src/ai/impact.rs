use crate::domain::{
    Chapter, NovelSettings, RewriteObligation, RewritePlan, SourceImpactLink, SourceImpactNode,
};
use crate::{
    format_planning_nodes, format_prior_contract_context, parse_jsonish_value,
    protagonist_rule_pack, DEEP_DELTA_CATEGORIES,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub(crate) const IMPACT_GRAPH_ASSET_KIND: &str = "主角性别影响图";
pub(crate) const REWRITE_CONTINUITY_ASSET_KIND: &str = "改写连续性状态";

pub(crate) fn parse_impact_nodes_from_analysis(
    analysis_json: &str,
    chapters: &[Chapter],
) -> Result<Vec<SourceImpactNode>, String> {
    let value: Value = serde_json::from_str(analysis_json)
        .map_err(|error| format!("主角影响节点 JSON 无效：{error}"))?;
    let candidates = value
        .get("protagonist_impact_nodes")
        .or_else(|| value.get("protagonist_nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    canonicalize_candidate_nodes(candidates, chapters)
}

fn canonicalize_candidate_nodes(
    candidates: Vec<Value>,
    chapters: &[Chapter],
) -> Result<Vec<SourceImpactNode>, String> {
    let chapters_by_index = chapters
        .iter()
        .map(|chapter| (chapter.index, chapter))
        .collect::<HashMap<_, _>>();
    let mut ordinals = HashMap::<i64, usize>::new();
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();

    for candidate in candidates {
        let mut node: SourceImpactNode = serde_json::from_value(candidate)
            .map_err(|error| format!("主角影响节点字段无效：{error}"))?;
        let chapter = chapters_by_index.get(&node.chapter_index).ok_or_else(|| {
            format!(
                "主角影响节点引用了当前分片之外的章节：{}",
                node.chapter_index
            )
        })?;
        validate_presence_kind(&node.presence_kind)?;
        if !node.confidence.is_finite() || !(0.0..=1.0).contains(&node.confidence) {
            return Err(format!(
                "章节 {} 的主角影响节点 confidence 必须在 0 到 1 之间。",
                node.chapter_index
            ));
        }
        if node.source_evidence.trim().is_empty()
            || !normalized_contains(&chapter.original_text, &node.source_evidence)
        {
            return Err(format!(
                "章节 {} 的主角影响节点证据无法在原文中定位：{}",
                chapter.index,
                node.source_evidence.trim()
            ));
        }
        if node.narrative_function.trim().is_empty() {
            return Err(format!(
                "章节 {} 的主角影响节点缺少叙事功能。",
                chapter.index
            ));
        }
        let ordinal = ordinals.entry(chapter.index).or_default();
        *ordinal += 1;
        node.ordinal = *ordinal;
        node.chapter_id = chapter.id.clone();
        node.node_id = stable_node_id(chapter, node.ordinal, &node.source_evidence);
        node.participants = normalized_nonempty(node.participants);
        node.gender_mechanisms = normalized_nonempty(node.gender_mechanisms);
        node.thread_keys = normalized_nonempty(node.thread_keys);
        node.links.retain(|link| {
            matches!(
                link.kind.as_str(),
                "causes" | "continues" | "pays_off" | "changes_state" | "same_thread"
            )
        });
        let dedup_key = format!(
            "{}:{}",
            node.chapter_id,
            normalize_evidence(&node.source_evidence)
        );
        if seen.insert(dedup_key) {
            nodes.push(node);
        }
    }
    nodes.sort_by_key(|node| (node.chapter_index, node.ordinal));
    Ok(nodes)
}

pub(crate) fn parse_impact_graph(content: &str) -> Vec<SourceImpactNode> {
    serde_json::from_str(content).unwrap_or_default()
}

pub(crate) fn serialize_impact_graph(nodes: &[SourceImpactNode]) -> Result<String, String> {
    serde_json::to_string(nodes).map_err(|error| error.to_string())
}

pub(crate) fn merge_impact_graph_nodes(
    existing: &[SourceImpactNode],
    incoming: &[SourceImpactNode],
) -> Vec<SourceImpactNode> {
    let mut merged = Vec::new();
    let mut seen = HashSet::new();
    for node in existing.iter().chain(incoming.iter()) {
        let key = format!(
            "{}:{}",
            node.chapter_id,
            normalize_evidence(&node.source_evidence)
        );
        if seen.insert(key) {
            merged.push(node.clone());
        }
    }
    merged.sort_by_key(|node| (node.chapter_index, node.ordinal));
    let lookup = merged.clone();
    let known_ids = lookup
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<HashSet<_>>();
    let mut last_by_thread = HashMap::<String, String>::new();
    for node in &mut merged {
        for link in &mut node.links {
            if !known_ids.contains(link.target.as_str()) {
                let normalized_target = normalize_evidence(&link.target);
                if let Some(target) = lookup.iter().rev().find(|candidate| {
                    candidate.chapter_index <= node.chapter_index
                        && candidate.node_id != node.node_id
                        && !candidate.source_evidence.trim().is_empty()
                        && normalized_target
                            .contains(&normalize_evidence(&candidate.source_evidence))
                }) {
                    link.target = target.node_id.clone();
                }
            }
        }
        node.links.retain(|link| {
            !link.target.trim().is_empty()
                && link.target != node.node_id
                && known_ids.contains(link.target.as_str())
        });
        for thread_key in &node.thread_keys {
            if let Some(previous) = last_by_thread.get(thread_key) {
                if !node
                    .links
                    .iter()
                    .any(|link| link.kind == "same_thread" && link.target == *previous)
                {
                    node.links.push(SourceImpactLink {
                        kind: "same_thread".to_string(),
                        target: previous.clone(),
                    });
                }
            }
            last_by_thread.insert(thread_key.clone(), node.node_id.clone());
        }
    }
    let positions = merged
        .iter()
        .enumerate()
        .map(|(index, node)| (node.node_id.clone(), index))
        .collect::<HashMap<_, _>>();
    let edges = merged
        .iter()
        .flat_map(|node| {
            node.links
                .iter()
                .map(|link| (node.node_id.clone(), link.kind.clone(), link.target.clone()))
        })
        .collect::<Vec<_>>();
    for node in &mut merged {
        node.links.clear();
    }
    for (source, kind, target) in edges {
        let (Some(&source_pos), Some(&target_pos)) =
            (positions.get(&source), positions.get(&target))
        else {
            continue;
        };
        let (from_pos, to_id) = if source_pos <= target_pos {
            (source_pos, target)
        } else {
            (target_pos, source)
        };
        let links = &mut merged[from_pos].links;
        if !links
            .iter()
            .any(|link| link.kind == kind && link.target == to_id)
        {
            links.push(SourceImpactLink {
                kind,
                target: to_id,
            });
        }
    }
    merged
}

pub(crate) fn impact_nodes_for_chapters(
    nodes: &[SourceImpactNode],
    chapters: &[Chapter],
) -> Vec<SourceImpactNode> {
    let indexes = chapters
        .iter()
        .map(|chapter| chapter.index)
        .collect::<HashSet<_>>();
    nodes
        .iter()
        .filter(|node| indexes.contains(&node.chapter_index))
        .cloned()
        .collect()
}

pub(crate) fn build_rewrite_plan_prompt(
    chapters: &[Chapter],
    nodes: &[SourceImpactNode],
    dependency_graph: &[SourceImpactNode],
    continuity_json: &str,
    settings: &NovelSettings,
    style_prompt: &str,
    prior_contracts: &[RewritePlan],
) -> String {
    let nodes_json = format_planning_nodes(nodes);
    let prior_contracts_json =
        format_prior_contract_context(prior_contracts, nodes, dependency_graph);
    let current_draft = if chapters.iter().any(|chapter| {
        chapter
            .rewrite_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    }) {
        crate::build_batch_chapter_text(chapters, true)
    } else {
        "无；这是从原文开始的全量规划。".to_string()
    };
    format!(
        r#"请为当前连续分片生成可执行的主角主动重构契约，只输出合法 JSON。

{}

硬性规划规则：
1. 当前分片中每个主角影响节点必须恰好对应一个 obligation；不能遗漏、合并或重复 node_id。
2. 每个 obligation 必须包含 R3_PROTAGONIST_NODE_DELTA，并至少选择一个深层变化类别。
3. 姓名、代词、称谓和外貌变化可以伴随出现，但不能单独作为 required_changes。
4. 即使节点不明显依赖性别，也要在不改变事件功能的前提下规划一个具体微变化，例如视角、动作表达、他人反应、交流方式或连续性呼应。
5. 保留原著事件、结果、能力、人物动机和关系性质；不得凭空增加恋爱对象、重大事件或剧情分支。
6. 复核原文是否漏掉主角直接出现、被提及或造成后果的节点；遗漏节点放入 graph_additions，并立即为其创建 obligation。graph_additions.node_id 使用 `new-章节index-序号`，对应 obligation.node_id 必须相同。
7. source_evidence 必须逐字摘自原文，不得概括或改写。
8. 如果提供“当前改写稿”，比较其与原文：已经满足深层变化的节点仍保留一项验收义务，并在 preserve 中写明保持现有处理；未满足节点和本次新要求进入修复义务，避免破坏已经成立的改写。
9. 阅读前序分片契约。如果当前义务依赖前序 node_id、obligation_id、thread_key 或计划状态，把稳定标识写入 cross_shard_dependencies；无依赖时返回空数组。

允许的深层变化类别：{}

只输出此结构：
{{
  "plan_version": "protagonist-graph-v1",
  "graph_additions": [],
  "obligations": [{{
    "obligation_id": "O-节点ID",
    "node_id": "节点ID",
    "chapter_index": 1,
    "rule_ids": ["R3_PROTAGONIST_NODE_DELTA", "R6_SOCIAL_CAUSALITY"],
    "preserve": ["必须保留的剧情功能"],
    "required_changes": ["必须在正文中可见的深层变化"],
    "deep_delta_categories": ["other_reaction"],
    "forbidden_regressions": ["不能引入的退化"],
    "downstream_effects": ["后续必须承接的影响"],
    "planned_state_updates": []
  }}],
  "planned_state_updates": [],
  "cross_shard_dependencies": []
}}

小说目标设定：
- 主角原名：{}
- 主角改写名：{}
- 主角别名/映射：{}
- 其他指定女性化角色：{}
- 重点互动对象：{}
- 身材/体型：{} / {}
- 模式：{}

全局风格补充（仅影响表达）：
{}

相关改写连续性状态（已合并通过状态与本批前序计划，只保留当前关系线）：
{}

前序分片依赖摘要（只含稳定 ID、章节和下游影响）：
{}

当前分析节点：
{}

当前改写稿（如有，仅用于单章基于改写稿重写的差异验收）：
{}

当前原文章节：
{}"#,
        protagonist_rule_pack(),
        DEEP_DELTA_CATEGORIES.join(", "),
        settings.protagonist_name.trim(),
        if settings.rewritten_protagonist_name.trim().is_empty() {
            "按姓名映射表生成"
        } else {
            settings.rewritten_protagonist_name.trim()
        },
        settings.protagonist_aliases.trim(),
        settings.additional_feminize_names.trim(),
        settings.relationship_targets.trim(),
        settings.bust.trim(),
        settings.body_type.trim(),
        settings.rewrite_mode.trim(),
        if style_prompt.trim().is_empty() {
            "无"
        } else {
            style_prompt.trim()
        },
        if continuity_json.trim().is_empty() {
            "[]"
        } else {
            continuity_json.trim()
        },
        prior_contracts_json,
        nodes_json,
        current_draft,
        crate::build_batch_chapter_text(chapters, false)
    )
}

pub(crate) fn parse_and_validate_rewrite_plan(
    output: &str,
    chapters: &[Chapter],
    base_nodes: &[SourceImpactNode],
) -> Result<RewritePlan, String> {
    let value = parse_jsonish_value(output)?;
    let mut plan: RewritePlan = serde_json::from_value(value)
        .map_err(|error| format!("改写契约 JSON 字段无效：{error}"))?;
    if plan.plan_version.trim() != "protagonist-graph-v1" {
        return Err("改写契约 plan_version 必须是 protagonist-graph-v1。".to_string());
    }
    let original_addition_ids = plan
        .graph_additions
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    let addition_values = plan
        .graph_additions
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    let additions = canonicalize_candidate_nodes(addition_values, chapters)?;
    let replacement_ids = original_addition_ids
        .into_iter()
        .zip(additions.iter().map(|node| node.node_id.clone()))
        .collect::<HashMap<_, _>>();
    for obligation in &mut plan.obligations {
        if let Some(replacement) = replacement_ids.get(&obligation.node_id) {
            obligation.node_id = replacement.clone();
            obligation.obligation_id = format!("O-{replacement}");
        }
    }
    plan.graph_additions = additions;

    let expected_nodes = base_nodes
        .iter()
        .chain(plan.graph_additions.iter())
        .map(|node| (node.node_id.clone(), node.chapter_index))
        .collect::<HashMap<_, _>>();
    let mut covered = HashSet::new();
    let mut obligation_ids = HashSet::new();
    for obligation in &plan.obligations {
        let expected_index = expected_nodes
            .get(&obligation.node_id)
            .ok_or_else(|| format!("改写契约引用了未知节点：{}", obligation.node_id))?;
        if obligation.chapter_index != *expected_index {
            return Err(format!(
                "义务 {} 的章节索引与节点不一致。",
                obligation.obligation_id
            ));
        }
        if !covered.insert(obligation.node_id.clone()) {
            return Err(format!("节点 {} 被多个义务重复覆盖。", obligation.node_id));
        }
        if !obligation_ids.insert(obligation.obligation_id.clone()) {
            return Err(format!(
                "改写契约重复使用 obligation_id：{}",
                obligation.obligation_id
            ));
        }
        validate_obligation(obligation)?;
    }
    let missing = expected_nodes
        .keys()
        .filter(|node_id| !covered.contains(*node_id))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!("改写契约遗漏主角节点：{}", missing.join("、")));
    }
    Ok(plan)
}

fn validate_obligation(obligation: &RewriteObligation) -> Result<(), String> {
    if obligation.obligation_id.trim().is_empty() {
        return Err("改写义务缺少 obligation_id。".to_string());
    }
    if !obligation
        .rule_ids
        .iter()
        .any(|rule| rule == "R3_PROTAGONIST_NODE_DELTA")
    {
        return Err(format!(
            "义务 {} 缺少 R3_PROTAGONIST_NODE_DELTA。",
            obligation.obligation_id
        ));
    }
    if obligation.required_changes.is_empty() {
        return Err(format!(
            "义务 {} 没有 required_changes。",
            obligation.obligation_id
        ));
    }
    if obligation.deep_delta_categories.is_empty()
        || obligation
            .deep_delta_categories
            .iter()
            .any(|category| !DEEP_DELTA_CATEGORIES.contains(&category.as_str()))
    {
        return Err(format!(
            "义务 {} 缺少合法的深层变化类别。",
            obligation.obligation_id
        ));
    }
    let joined = obligation.required_changes.join("");
    let surface_terms = [
        "改名", "姓名", "代词", "称谓", "外貌", "发丝", "衣裙", "身材",
    ];
    let deep_terms = [
        "自我认知",
        "心理",
        "反应",
        "互动",
        "边界",
        "对话",
        "语气",
        "名声",
        "评价",
        "因果",
        "能力逻辑",
        "关系",
        "张力",
        "动作表达",
        "误会",
        "幽默",
        "呼应",
        "承诺",
        "距离",
        "交流方式",
        "视角",
        "保护方式",
        "处境",
    ];
    if surface_terms.iter().any(|term| joined.contains(term))
        && !deep_terms.iter().any(|term| joined.contains(term))
    {
        return Err(format!(
            "义务 {} 只描述了表层姓名或外貌修改。",
            obligation.obligation_id
        ));
    }
    Ok(())
}

pub(crate) fn format_rewrite_contract(plan: &RewritePlan) -> String {
    serde_json::to_string_pretty(plan).unwrap_or_else(|_| "{}".to_string())
}

pub(crate) fn parse_rewrite_output_envelope(output: &str, tagged: bool) -> Result<String, String> {
    if !tagged {
        return Ok(output.to_string());
    }
    for tag in [
        "<rewrite_check>",
        "</rewrite_check>",
        "<output>",
        "</output>",
    ] {
        if output.matches(tag).count() != 1 {
            return Err(format!("短自检标签 {tag} 缺失或重复。"));
        }
    }
    let check_start = output
        .find("<rewrite_check>")
        .ok_or_else(|| "已开启短自检，但模型输出缺少 <rewrite_check>。".to_string())?;
    let check_end = output
        .find("</rewrite_check>")
        .ok_or_else(|| "已开启短自检，但模型输出缺少 </rewrite_check>。".to_string())?;
    let output_start = output
        .find("<output>")
        .ok_or_else(|| "已开启短自检，但模型输出缺少 <output>。".to_string())?;
    let output_end = output
        .rfind("</output>")
        .ok_or_else(|| "已开启短自检，但模型输出缺少 </output>。".to_string())?;
    if check_start != output.rfind("<rewrite_check>").unwrap_or(check_start)
        || output_start != output.rfind("<output>").unwrap_or(output_start)
        || check_end >= output_start
        || output_start >= output_end
    {
        return Err("短自检标签重复或顺序无效。".to_string());
    }
    let prefix = output[..check_start].trim();
    let between = output[check_end + "</rewrite_check>".len()..output_start].trim();
    let suffix = output[output_end + "</output>".len()..].trim();
    if !prefix.is_empty() || !between.is_empty() || !suffix.is_empty() {
        return Err("短自检标签外包含额外正文。".to_string());
    }
    let body = output[output_start + "<output>".len()..output_end].trim();
    if [
        "<rewrite_check>",
        "</rewrite_check>",
        "<output>",
        "</output>",
    ]
    .iter()
    .any(|tag| body.contains(tag))
    {
        return Err("短自检标签泄漏到正文区域。".to_string());
    }
    Ok(body.to_string())
}

fn stable_node_id(chapter: &Chapter, ordinal: usize, evidence: &str) -> String {
    let digest = Sha256::digest(normalize_evidence(evidence).as_bytes());
    let short = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("impact-{}-{ordinal}-{short}", chapter.id)
}

fn normalized_contains(haystack: &str, needle: &str) -> bool {
    let normalized_needle = normalize_evidence(needle);
    !normalized_needle.is_empty() && normalize_evidence(haystack).contains(&normalized_needle)
}

fn normalize_evidence(value: &str) -> String {
    value.chars().filter(|ch| !ch.is_whitespace()).collect()
}

fn normalized_nonempty(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && seen.insert(value.clone()))
        .collect()
}

fn validate_presence_kind(kind: &str) -> Result<(), String> {
    if matches!(kind, "direct" | "mentioned" | "consequence") {
        Ok(())
    } else {
        Err(format!("未知的主角节点 presence_kind：{kind}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter() -> Chapter {
        Chapter {
            id: "c1".to_string(),
            novel_id: "n1".to_string(),
            index: 1,
            title: "第一章".to_string(),
            original_text: "萧炎推门进来，药老看了他一眼。".to_string(),
            analysis_json: None,
            rewrite_text: None,
            rewrite_edited: false,
            single_rewrite_original_available: false,
            analysis_status: "completed".to_string(),
            rewrite_status: "pending".to_string(),
            rewrite_validation_status: "unvalidated".to_string(),
            rewrite_obligation_total: 0,
            rewrite_obligation_satisfied: 0,
        }
    }

    #[test]
    fn analysis_nodes_require_real_source_evidence_and_get_stable_ids() {
        let json = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index": 1,
            "presence_kind": "direct",
            "participants": ["萧炎", "药老"],
            "source_evidence": "萧炎推门进来",
            "narrative_function": "主角入场",
            "gender_mechanisms": [],
            "thread_keys": ["主角-药老"],
            "links": [],
            "confidence": 0.9
          }]
        }"#;
        let nodes = parse_impact_nodes_from_analysis(json, &[chapter()]).expect("valid nodes");
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].node_id.starts_with("impact-c1-1-"));
    }

    #[test]
    fn tagged_envelope_keeps_only_output_body() {
        let output = "<rewrite_check>已核对</rewrite_check>\n<output>正文</output>";
        assert_eq!(parse_rewrite_output_envelope(output, true).unwrap(), "正文");
        assert!(parse_rewrite_output_envelope("<output>正文</output>", true).is_err());
        assert!(parse_rewrite_output_envelope(
            "<rewrite_check>已核对</rewrite_check></rewrite_check><output>正文</output>",
            true,
        )
        .is_err());
    }

    #[test]
    fn plan_requires_exactly_one_deep_obligation_per_node() {
        let chapter = chapter();
        let nodes = parse_impact_nodes_from_analysis(
            r#"{"protagonist_impact_nodes":[{"chapter_index":1,"presence_kind":"direct","source_evidence":"萧炎推门进来","narrative_function":"入场"}]}"#,
            std::slice::from_ref(&chapter),
        )
        .unwrap();
        let valid = format!(
            r#"{{"plan_version":"protagonist-graph-v1","obligations":[{{"obligation_id":"O-1","node_id":"{}","chapter_index":1,"rule_ids":["R3_PROTAGONIST_NODE_DELTA"],"required_changes":["让药老对她的入场方式产生可见反应"],"deep_delta_categories":["other_reaction"]}}]}}"#,
            nodes[0].node_id
        );
        assert!(
            parse_and_validate_rewrite_plan(&valid, std::slice::from_ref(&chapter), &nodes).is_ok()
        );

        let surface_only =
            valid.replace("让药老对她的入场方式产生可见反应", "只修改姓名、代词和外貌");
        assert!(parse_and_validate_rewrite_plan(
            &surface_only,
            std::slice::from_ref(&chapter),
            &nodes,
        )
        .is_err());
    }

    #[test]
    fn graph_merge_deduplicates_same_source_evidence() {
        let chapter = chapter();
        let nodes = parse_impact_nodes_from_analysis(
            r#"{"protagonist_impact_nodes":[{"chapter_index":1,"presence_kind":"direct","source_evidence":"萧炎推门进来","narrative_function":"入场"}]}"#,
            std::slice::from_ref(&chapter),
        )
        .unwrap();
        assert_eq!(merge_impact_graph_nodes(&nodes, &nodes).len(), 1);
    }

    #[test]
    fn graph_merge_orients_same_thread_edges_forward() {
        let mut first = parse_impact_nodes_from_analysis(
            r#"{"protagonist_impact_nodes":[{"chapter_index":1,"presence_kind":"direct","source_evidence":"萧炎推门进来","narrative_function":"入场","thread_keys":["师徒"]}]}"#,
            &[chapter()],
        )
        .unwrap()
        .remove(0);
        first.node_id = "N-1".to_string();
        let mut second = first.clone();
        second.node_id = "N-2".to_string();
        second.chapter_id = "c2".to_string();
        second.chapter_index = 2;
        second.source_evidence = "药老再次看向她".to_string();
        let merged = merge_impact_graph_nodes(&[first], &[second]);
        assert!(merged[0]
            .links
            .iter()
            .any(|link| link.kind == "same_thread" && link.target == "N-2"));
        assert!(merged[1].links.is_empty());
    }
}
