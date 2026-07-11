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
    let chapter_indexes = chapters
        .iter()
        .map(|chapter| chapter.index)
        .collect::<HashSet<_>>();
    let uses_local_chapter_ordinals = candidates.iter().any(|candidate| {
        candidate
            .get("chapter_index")
            .and_then(Value::as_i64)
            .is_some_and(|index| {
                !chapter_indexes.contains(&index)
                    && index >= 1
                    && usize::try_from(index).is_ok_and(|index| index <= chapters.len())
            })
    });
    let mut ordinals = HashMap::<i64, usize>::new();
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();

    for candidate in candidates {
        let mut node: SourceImpactNode = serde_json::from_value(candidate)
            .map_err(|error| format!("主角影响节点字段无效：{error}"))?;
        validate_presence_kind(&node.presence_kind)?;
        if !node.confidence.is_finite() || !(0.0..=1.0).contains(&node.confidence) {
            return Err(format!(
                "章节 {} 的主角影响节点 confidence 必须在 0 到 1 之间。",
                node.chapter_index
            ));
        }
        let submitted_evidence = node.source_evidence.trim().to_string();
        let (chapter, source_evidence) = resolve_source_chapter(
            chapters,
            node.chapter_index,
            &submitted_evidence,
            uses_local_chapter_ordinals,
        )?;
        node.chapter_index = chapter.index;
        node.source_evidence = source_evidence;
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

fn resolve_source_chapter<'a>(
    chapters: &'a [Chapter],
    submitted_chapter_index: i64,
    submitted_evidence: &str,
    uses_local_chapter_ordinals: bool,
) -> Result<(&'a Chapter, String), String> {
    let matches = chapters
        .iter()
        .filter_map(|chapter| {
            resolve_source_evidence(&chapter.original_text, submitted_evidence)
                .map(|evidence| (chapter, evidence))
        })
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(format!(
            "模型标注章节 {} 的主角影响节点证据无法在当前分片任何章节中定位可靠原文锚点：{}",
            submitted_chapter_index, submitted_evidence
        ));
    }
    if matches.len() == 1 {
        return Ok(matches.into_iter().next().expect("one evidence match"));
    }

    let local_chapter = submitted_chapter_index
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .and_then(|index| chapters.get(index));
    let absolute_chapter = chapters
        .iter()
        .find(|chapter| chapter.index == submitted_chapter_index);
    let preferred = if uses_local_chapter_ordinals {
        local_chapter.or(absolute_chapter)
    } else {
        absolute_chapter.or(local_chapter)
    };
    if let Some(preferred) = preferred {
        if let Some((chapter, evidence)) = matches
            .iter()
            .find(|(chapter, _)| chapter.id == preferred.id)
        {
            return Ok((*chapter, evidence.clone()));
        }
    }

    Err(format!(
        "模型标注章节 {} 的主角影响节点证据同时匹配当前分片多个章节，无法安全归属：{}",
        submitted_chapter_index, submitted_evidence
    ))
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
9. 阅读前序分片契约。如果当前义务依赖前序 node_id、obligation_id、thread_key 或计划状态，把稳定标识写入 cross_shard_dependencies。该字段必须是扁平字符串数组（例如 ["obligation:O-xxx", "thread:师徒"]），严禁输出对象；无依赖时返回空数组。
10. 每个 planned_state_updates 项必须包含 thread_key、state_type、value、当前分片 chapter_index 和非空 source_obligation_ids；义务内部的状态必须把该义务自身 ID 列为来源。

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
    "planned_state_updates": [{{
      "thread_key": "关系线",
      "state_type": "边界或承诺类型",
      "value": "本章建立的最新状态",
      "chapter_index": 1,
      "source_obligation_ids": ["O-节点ID"]
    }}]
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
    let mut value = parse_jsonish_value(output)?;
    normalize_cross_shard_dependencies(&mut value)?;
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
    let mut obligation_replacements = HashMap::new();
    for obligation in &mut plan.obligations {
        if let Some(replacement) = replacement_ids.get(&obligation.node_id) {
            let previous_obligation_id = obligation.obligation_id.clone();
            obligation.node_id = replacement.clone();
            obligation.obligation_id = format!("O-{replacement}");
            obligation_replacements
                .insert(previous_obligation_id, obligation.obligation_id.clone());
        }
    }
    for state in plan.planned_state_updates.iter_mut().chain(
        plan.obligations
            .iter_mut()
            .flat_map(|obligation| obligation.planned_state_updates.iter_mut()),
    ) {
        for source_id in &mut state.source_obligation_ids {
            if let Some(replacement) = obligation_replacements.get(source_id) {
                *source_id = replacement.clone();
            }
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
    validate_plan_state_updates(&plan, chapters)?;
    Ok(plan)
}

fn normalize_cross_shard_dependencies(value: &mut serde_json::Value) -> Result<(), String> {
    let Some(dependencies) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("cross_shard_dependencies"))
    else {
        return Ok(());
    };
    let Some(items) = dependencies.as_array_mut() else {
        return Err("cross_shard_dependencies 必须是字符串数组。".to_string());
    };

    for item in items {
        if item.is_string() {
            continue;
        }
        let object = item
            .as_object()
            .ok_or_else(|| "cross_shard_dependencies 只能包含稳定 ID 字符串。".to_string())?;
        let dependency = [
            ("obligation_id", "obligation:"),
            ("node_id", "node:"),
            ("thread_key", "thread:"),
            ("dependency_id", ""),
            ("depends_on", ""),
            ("id", ""),
        ]
        .iter()
        .find_map(|(key, prefix)| {
            object
                .get(*key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| format!("{prefix}{value}"))
        })
        .ok_or_else(|| {
            "cross_shard_dependencies 对象缺少 obligation_id、node_id 或 thread_key 稳定标识。"
                .to_string()
        })?;
        *item = serde_json::Value::String(dependency);
    }
    Ok(())
}

fn validate_plan_state_updates(plan: &RewritePlan, chapters: &[Chapter]) -> Result<(), String> {
    let chapter_indexes = chapters
        .iter()
        .map(|chapter| chapter.index)
        .collect::<HashSet<_>>();
    let obligation_ids = plan
        .obligations
        .iter()
        .map(|obligation| obligation.obligation_id.as_str())
        .collect::<HashSet<_>>();
    for (owner, state) in plan
        .planned_state_updates
        .iter()
        .map(|state| (None, state))
        .chain(plan.obligations.iter().flat_map(|obligation| {
            obligation
                .planned_state_updates
                .iter()
                .map(move |state| (Some(obligation.obligation_id.as_str()), state))
        }))
    {
        if state.thread_key.trim().is_empty()
            || state.state_type.trim().is_empty()
            || state.value.trim().is_empty()
        {
            return Err("计划状态必须包含 thread_key、state_type 和 value。".to_string());
        }
        if !chapter_indexes.contains(&state.chapter_index) {
            return Err(format!(
                "计划状态 {} / {} 引用了当前分片之外的章节 {}。",
                state.thread_key, state.state_type, state.chapter_index
            ));
        }
        let normalized_source_count = state
            .source_obligation_ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect::<HashSet<_>>()
            .len();
        if state.source_obligation_ids.is_empty()
            || normalized_source_count != state.source_obligation_ids.len()
            || state
                .source_obligation_ids
                .iter()
                .any(|id| !obligation_ids.contains(id.trim()))
        {
            return Err(format!(
                "计划状态 {} / {} 缺少合法来源义务 ID。",
                state.thread_key, state.state_type
            ));
        }
        if let Some(owner) = owner {
            if !state
                .source_obligation_ids
                .iter()
                .any(|id| id.trim() == owner)
            {
                return Err(format!(
                    "义务 {owner} 的计划状态没有把自身列入 source_obligation_ids。"
                ));
            }
        }
    }
    Ok(())
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

const MIN_PUNCTUATION_INSENSITIVE_EVIDENCE_CHARS: usize = 8;
const MIN_FALLBACK_EVIDENCE_FRAGMENT_CHARS: usize = 12;
const MIN_CHAIN_EVIDENCE_FRAGMENT_CHARS: usize = 4;
const MIN_CHAIN_EVIDENCE_TOTAL_CHARS: usize = 16;
const MAX_CHAIN_EVIDENCE_SPAN_CHARS: usize = 120;

#[derive(Clone, Copy)]
struct ResolvedEvidenceAnchor {
    start: usize,
    end: usize,
    length: usize,
}

fn resolve_source_evidence(source: &str, submitted: &str) -> Option<String> {
    let submitted = submitted.trim();
    if submitted.is_empty() {
        return None;
    }
    if source.contains(submitted) {
        return Some(submitted.to_string());
    }

    if let Some((start, end)) = locate_normalized_span(source, submitted, false, false) {
        return Some(source[start..end].trim().to_string());
    }

    let submitted_anchor = normalize_evidence_anchor(submitted);
    if submitted_anchor.len() >= MIN_PUNCTUATION_INSENSITIVE_EVIDENCE_CHARS {
        if let Some((start, end)) = locate_normalized_span(source, submitted, true, true) {
            return Some(expand_source_evidence_end(source, start, end));
        }
    }

    if let Some((start, end)) = locate_ordered_fragment_chain(source, submitted) {
        return Some(expand_source_evidence_end(source, start, end));
    }

    let mut fragments = submitted
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|fragment| {
            normalize_evidence_anchor(fragment).len() >= MIN_FALLBACK_EVIDENCE_FRAGMENT_CHARS
        })
        .collect::<Vec<_>>();
    fragments.sort_by_key(|fragment| std::cmp::Reverse(normalize_evidence_anchor(fragment).len()));
    fragments.dedup();
    for fragment in fragments {
        if let Some((start, end)) = locate_normalized_span(source, fragment, true, true) {
            return Some(expand_source_evidence_end(source, start, end));
        }
    }
    None
}

fn locate_ordered_fragment_chain(source: &str, submitted: &str) -> Option<(usize, usize)> {
    let anchors = submitted
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter_map(|fragment| {
            let length = normalize_evidence_anchor(fragment).len();
            if length < MIN_CHAIN_EVIDENCE_FRAGMENT_CHARS {
                return None;
            }
            locate_normalized_span(source, fragment, true, true).map(|(start, end)| {
                ResolvedEvidenceAnchor { start, end, length }
            })
        })
        .collect::<Vec<_>>();

    let mut best = None::<(usize, usize, usize, usize)>;
    for (index, first) in anchors.iter().enumerate() {
        let mut end = first.end;
        let mut total = first.length;
        let mut count = 1;
        let mut strongest = first.length;
        for anchor in anchors.iter().skip(index + 1) {
            if anchor.start < end
                || source[first.start..anchor.end].chars().count()
                    > MAX_CHAIN_EVIDENCE_SPAN_CHARS
            {
                continue;
            }
            end = anchor.end;
            total += anchor.length;
            count += 1;
            strongest = strongest.max(anchor.length);
        }
        if count < 2
            || total < MIN_CHAIN_EVIDENCE_TOTAL_CHARS
            || (strongest < MIN_PUNCTUATION_INSENSITIVE_EVIDENCE_CHARS && count < 3)
        {
            continue;
        }
        let should_replace = best
            .as_ref()
            .is_none_or(|(_, _, best_total, best_count)| {
                total > *best_total || (total == *best_total && count > *best_count)
            });
        if should_replace {
            best = Some((first.start, end, total, count));
        }
    }
    best.map(|(start, end, _, _)| (start, end))
}

fn locate_normalized_span(
    source: &str,
    submitted: &str,
    ignore_punctuation: bool,
    require_unique: bool,
) -> Option<(usize, usize)> {
    let (source_chars, source_ranges) = normalize_evidence_with_ranges(source, ignore_punctuation);
    let (submitted_chars, _) = normalize_evidence_with_ranges(submitted, ignore_punctuation);
    if submitted_chars.is_empty() || submitted_chars.len() > source_chars.len() {
        return None;
    }

    let mut matched_start = None;
    for start in 0..=source_chars.len() - submitted_chars.len() {
        if source_chars[start..start + submitted_chars.len()] == submitted_chars {
            if require_unique && matched_start.is_some() {
                return None;
            }
            matched_start = Some(start);
            if !require_unique {
                break;
            }
        }
    }
    let start = matched_start?;
    Some((
        source_ranges[start].0,
        source_ranges[start + submitted_chars.len() - 1].1,
    ))
}

fn normalize_evidence_with_ranges(
    value: &str,
    ignore_punctuation: bool,
) -> (Vec<char>, Vec<(usize, usize)>) {
    let mut normalized = Vec::new();
    let mut ranges = Vec::new();
    for (start, character) in value.char_indices() {
        if character.is_whitespace() || (ignore_punctuation && !character.is_alphanumeric()) {
            continue;
        }
        normalized.push(if character.is_ascii() {
            character.to_ascii_lowercase()
        } else {
            character
        });
        ranges.push((start, start + character.len_utf8()));
    }
    (normalized, ranges)
}

fn normalize_evidence_anchor(value: &str) -> Vec<char> {
    normalize_evidence_with_ranges(value, true).0
}

fn expand_source_evidence_end(source: &str, start: usize, end: usize) -> String {
    let mut expanded_end = end;
    for (offset, character) in source[end..].char_indices() {
        if !matches!(
            character,
            '。' | '！' | '？' | '!' | '?' | '…' | '”' | '’' | '"' | '\'' | '）' | ')' | '】' | ']' | '》' | '〉'
        ) {
            break;
        }
        expanded_end = end + offset + character.len_utf8();
    }
    source[start..expanded_end].trim().to_string()
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
    use crate::domain::{ParsedChapterRewrite, ReviewCoverageItem, RewriteStateUpdate};

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

    fn indexed_chapter(index: i64, original_text: &str) -> Chapter {
        let mut chapter = chapter();
        chapter.id = format!("c{index}");
        chapter.index = index;
        chapter.title = format!("第{index}章");
        chapter.original_text = original_text.to_string();
        chapter
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
    fn analysis_nodes_infer_absolute_chapters_from_local_shard_ordinals() {
        let chapters = [
            indexed_chapter(8, "吉尔伽美什坐在王座上回忆智慧巨兽。"),
            indexed_chapter(9, "许纸正在吃饭，并不想理会他的祈求。"),
            indexed_chapter(10, "许纸放下橙子，慢慢大步走进了沙盘。"),
        ];
        let json = r#"{
          "protagonist_impact_nodes": [
            {"chapter_index":1,"presence_kind":"mentioned","source_evidence":"吉尔伽美什坐在王座上回忆智慧巨兽","narrative_function":"回忆造物主"},
            {"chapter_index":2,"presence_kind":"direct","source_evidence":"许纸正在吃饭，并不想理会他的祈求","narrative_function":"暂不介入"},
            {"chapter_index":3,"presence_kind":"direct","source_evidence":"许纸放下橙子，慢慢大步走进了沙盘","narrative_function":"进入沙盘"}
          ]
        }"#;

        let nodes = parse_impact_nodes_from_analysis(json, &chapters)
            .expect("source evidence should determine absolute chapter indexes");

        assert_eq!(
            nodes
                .iter()
                .map(|node| node.chapter_index)
                .collect::<Vec<_>>(),
            [8, 9, 10]
        );
        assert_eq!(
            nodes
                .iter()
                .map(|node| node.chapter_id.as_str())
                .collect::<Vec<_>>(),
            ["c8", "c9", "c10"]
        );
    }

    #[test]
    fn analysis_node_uses_unique_evidence_over_a_wrong_absolute_index() {
        let chapters = [
            indexed_chapter(8, "吉尔伽美什回忆智慧巨兽。"),
            indexed_chapter(9, "许纸正在吃饭，并不想理会他。"),
        ];
        let json = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index":8,
            "presence_kind":"direct",
            "source_evidence":"许纸正在吃饭，并不想理会他",
            "narrative_function":"暂不介入"
          }]
        }"#;

        let nodes = parse_impact_nodes_from_analysis(json, &chapters)
            .expect("unique source evidence should correct the model index");

        assert_eq!(nodes[0].chapter_index, 9);
        assert_eq!(nodes[0].chapter_id, "c9");
    }

    #[test]
    fn analysis_node_rejects_ambiguous_evidence_without_a_safe_chapter_hint() {
        let chapters = [
            indexed_chapter(8, "许纸点点头，随后离开。"),
            indexed_chapter(9, "许纸点点头，随后离开。"),
        ];
        let json = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index":99,
            "presence_kind":"direct",
            "source_evidence":"许纸点点头，随后离开",
            "narrative_function":"离场"
          }]
        }"#;

        let error = parse_impact_nodes_from_analysis(json, &chapters)
            .expect_err("ambiguous evidence must not be assigned arbitrarily");

        assert!(error.contains("同时匹配当前分片多个章节"));
    }

    #[test]
    fn analysis_evidence_recovers_adjacent_source_text_with_changed_quotes() {
        let mut chapter = chapter();
        chapter.original_text =
            "在下午五点多，许纸倒是忽然被叫住了：\r\n　　“喂，你是许纸？？”\r\n　　许纸扭头。"
                .to_string();
        let json = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index": 1,
            "presence_kind": "direct",
            "source_evidence": "“许纸倒是忽然被叫住了：'喂，你是许纸？'”",
            "narrative_function": "引出旧识重逢"
          }]
        }"#;

        let nodes = parse_impact_nodes_from_analysis(json, std::slice::from_ref(&chapter))
            .expect("punctuation-only drift should resolve locally");

        assert_eq!(
            nodes[0].source_evidence,
            "许纸倒是忽然被叫住了：\r\n　　“喂，你是许纸？？”"
        );
        assert!(chapter.original_text.contains(&nodes[0].source_evidence));
    }

    #[test]
    fn analysis_evidence_reduces_stitched_paraphrase_to_unique_source_anchor() {
        let mut chapter = chapter();
        chapter.original_text = "许纸深呼吸一口气，觉得应该捏死它。可是想想还是算了，毕竟优胜劣汰了那么多，才诞生一个适者生存的变异种，现在杀不得。".to_string();
        let json = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index": 1,
            "presence_kind": "direct",
            "source_evidence": "许纸深呼吸一口气，觉得应该捏死它，但想想还是算了，毕竟优胜劣汰了那么多，才诞生一个适者生存的变异种",
            "narrative_function": "决定保留变异种"
          }]
        }"#;

        let nodes = parse_impact_nodes_from_analysis(json, std::slice::from_ref(&chapter))
            .expect("a long unique exact fragment should remain a reliable anchor");

        assert!(chapter.original_text.contains(&nodes[0].source_evidence));
        assert!(nodes[0].source_evidence.contains("适者生存的变异种"));
    }

    #[test]
    fn analysis_evidence_recovers_a_contiguous_source_span_around_an_omitted_clause() {
        let mut chapter = chapter();
        chapter.original_text = "“这样下去，灭绝是肯定不行的。”\r\n　　许纸想到这，微微面色一动，回到屋里，登上笔记本电脑，打开无线网络，上淘宝定制了一些东西，“看来，得想办法了！”".to_string();
        let json = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index": 1,
            "presence_kind": "direct",
            "source_evidence": "“这样下去，灭绝是肯定不行的。”许纸想到这，微微面色一动，回到屋里，登上笔记本电脑，上淘宝定制了一些东西",
            "narrative_function": "决定为虫猿准备文明火种"
          }]
        }"#;

        let nodes = parse_impact_nodes_from_analysis(json, std::slice::from_ref(&chapter))
            .expect("multiple unique ordered anchors should recover the omitted source clause");

        assert!(chapter.original_text.contains(&nodes[0].source_evidence));
        assert!(nodes[0].source_evidence.contains("打开无线网络"));
        assert!(nodes[0].source_evidence.ends_with("上淘宝定制了一些东西"));
    }

    #[test]
    fn analysis_evidence_still_rejects_fabricated_or_ambiguous_anchors() {
        let mut chapter = chapter();
        chapter.original_text = "许纸缓缓推开大门走了进来。许纸缓缓推开大门走了进来。".to_string();
        let fabricated = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index": 1,
            "presence_kind": "direct",
            "source_evidence": "许纸从窗外飞了进来",
            "narrative_function": "伪造事件"
          }]
        }"#;
        let ambiguous = r#"{
          "protagonist_impact_nodes": [{
            "chapter_index": 1,
            "presence_kind": "direct",
            "source_evidence": "“许纸缓缓推开大门走了进来”",
            "narrative_function": "重复场景"
          }]
        }"#;

        assert!(
            parse_impact_nodes_from_analysis(fabricated, std::slice::from_ref(&chapter)).is_err()
        );
        assert!(
            parse_impact_nodes_from_analysis(ambiguous, std::slice::from_ref(&chapter)).is_err()
        );
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
    fn planner_normalizes_object_dependencies_without_another_model_call() {
        let chapter = chapter();
        let nodes = parse_impact_nodes_from_analysis(
            r#"{"protagonist_impact_nodes":[{"chapter_index":1,"presence_kind":"direct","source_evidence":"萧炎推门进来","narrative_function":"入场"}]}"#,
            std::slice::from_ref(&chapter),
        )
        .unwrap();
        let output = format!(
            r#"{{"plan_version":"protagonist-graph-v1","obligations":[{{"obligation_id":"O-1","node_id":"{}","chapter_index":1,"rule_ids":["R3_PROTAGONIST_NODE_DELTA"],"required_changes":["让药老对她的入场方式产生可见反应"],"deep_delta_categories":["other_reaction"]}}],"cross_shard_dependencies":[{{"obligation_id":"O-prior","reason":"承接前序状态"}}]}}"#,
            nodes[0].node_id
        );

        let plan = parse_and_validate_rewrite_plan(&output, std::slice::from_ref(&chapter), &nodes)
            .unwrap();

        assert_eq!(
            plan.cross_shard_dependencies,
            vec!["obligation:O-prior".to_string()]
        );
    }

    #[test]
    fn planner_rejects_dependency_objects_without_a_stable_identifier() {
        let output = r#"{"plan_version":"protagonist-graph-v1","cross_shard_dependencies":[{"reason":"承接前序状态"}]}"#;
        let error = parse_and_validate_rewrite_plan(output, &[], &[]).unwrap_err();
        assert!(error.contains("缺少 obligation_id、node_id 或 thread_key"));
    }

    #[test]
    fn planner_addition_gets_stable_ids_and_remaps_state_provenance() {
        let chapter = chapter();
        let output = r#"{
          "plan_version":"protagonist-graph-v1",
          "graph_additions":[{
            "node_id":"new-1-1",
            "chapter_index":1,
            "presence_kind":"mentioned",
            "source_evidence":"药老看了他一眼",
            "narrative_function":"他人观察",
            "confidence":0.9
          }],
          "obligations":[{
            "obligation_id":"O-new-1-1",
            "node_id":"new-1-1",
            "chapter_index":1,
            "rule_ids":["R3_PROTAGONIST_NODE_DELTA"],
            "required_changes":["让药老的观察反应产生可见差异"],
            "deep_delta_categories":["other_reaction"],
            "planned_state_updates":[{
              "thread_key":"师徒",
              "state_type":"观察",
              "value":"药老开始留意她的表达",
              "chapter_index":1,
              "source_obligation_ids":["O-new-1-1"]
            }]
          }]
        }"#;

        let plan = parse_and_validate_rewrite_plan(output, &[chapter], &[]).unwrap();

        assert!(plan.graph_additions[0].node_id.starts_with("impact-c1-1-"));
        assert_eq!(
            plan.obligations[0].planned_state_updates[0].source_obligation_ids,
            vec![plan.obligations[0].obligation_id.clone()]
        );
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

    #[test]
    fn protagonist_scenario_matrix_reaches_contract_and_coverage_gate() {
        let mut chapter = chapter();
        chapter.original_text = [
            "萧炎和林动勾肩搭背地走进院子。",
            "薰儿握住萧炎的手，没有松开。",
            "萧炎挡在众人之前，催动异火。",
            "旁人都说萧炎是个不好招惹的少年。",
            "长老们在议事厅提起萧炎的婚约。",
            "即使萧炎不在，昨日的决定仍让众人改变行程。",
            "萧炎想起自己答应薰儿不再隐瞒。",
        ]
        .join("");
        let evidences = [
            "萧炎和林动勾肩搭背地走进院子",
            "薰儿握住萧炎的手，没有松开",
            "萧炎挡在众人之前，催动异火",
            "旁人都说萧炎是个不好招惹的少年",
            "长老们在议事厅提起萧炎的婚约",
            "即使萧炎不在，昨日的决定仍让众人改变行程",
            "萧炎想起自己答应薰儿不再隐瞒",
        ];
        let presence = [
            "direct",
            "direct",
            "direct",
            "mentioned",
            "mentioned",
            "consequence",
            "direct",
        ];
        let candidates = evidences
            .iter()
            .zip(presence)
            .enumerate()
            .map(|(index, (evidence, presence_kind))| {
                serde_json::json!({
                    "chapter_index": 1,
                    "presence_kind": presence_kind,
                    "participants": ["萧炎"],
                    "source_evidence": evidence,
                    "narrative_function": format!("场景功能-{index}"),
                    "thread_keys": [format!("关系线-{index}")],
                    "confidence": 0.95
                })
            })
            .collect::<Vec<_>>();
        let analysis = serde_json::json!({"protagonist_impact_nodes": candidates}).to_string();
        let nodes = parse_impact_nodes_from_analysis(&analysis, std::slice::from_ref(&chapter))
            .expect("scenario nodes");
        assert_eq!(nodes.len(), 7);
        assert!(parse_impact_nodes_from_analysis(
            r#"{"protagonist_impact_nodes":[]}"#,
            std::slice::from_ref(&chapter)
        )
        .unwrap()
        .is_empty());

        let categories = [
            "interaction_boundary",
            "relationship_tension",
            "power_rationale",
            "social_reputation",
            "other_reaction",
            "action_expression",
            "continuity_callback",
        ];
        let changes = [
            "重写与男性同伴的互动边界和身体距离",
            "让既有女性关系的张力在握手反应中自然显现",
            "保持异火能力并调整保护动作表达与能力逻辑",
            "让旁人评价形成新的社会名声反应",
            "让婚约议论体现他人反应而不改变关系性质",
            "让主角缺席后果通过众人的行动表达出来",
            "回收对薰儿的承诺并形成连续性呼应",
        ];
        let obligations = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| RewriteObligation {
                obligation_id: format!("O-{index}"),
                node_id: node.node_id.clone(),
                chapter_index: 1,
                rule_ids: vec!["R3_PROTAGONIST_NODE_DELTA".to_string()],
                preserve: vec![node.narrative_function.clone()],
                required_changes: vec![changes[index].to_string()],
                deep_delta_categories: vec![categories[index].to_string()],
                forbidden_regressions: Vec::new(),
                downstream_effects: Vec::new(),
                planned_state_updates: if index == 6 {
                    vec![RewriteStateUpdate {
                        thread_key: "主角-薰儿承诺".to_string(),
                        state_type: "承诺".to_string(),
                        value: "不再隐瞒".to_string(),
                        chapter_index: 1,
                        source_obligation_ids: vec!["O-6".to_string()],
                    }]
                } else {
                    Vec::new()
                },
            })
            .collect::<Vec<_>>();
        let plan = RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations,
            planned_state_updates: Vec::new(),
            cross_shard_dependencies: Vec::new(),
        };
        let parsed_plan = parse_and_validate_rewrite_plan(
            &serde_json::to_string(&plan).unwrap(),
            std::slice::from_ref(&chapter),
            &nodes,
        )
        .expect("scenario contract");
        let rewrite = ParsedChapterRewrite {
            id: chapter.id.clone(),
            index: 1,
            title: chapter.title.clone(),
            text: (0..7)
                .map(|index| format!("完成义务证据-{index}"))
                .collect::<Vec<_>>()
                .join("；"),
        };
        let coverage = (0..7)
            .map(|index| ReviewCoverageItem {
                obligation_id: format!("O-{index}"),
                status: "satisfied".to_string(),
                chapter_indexes: vec![1],
                evidence: format!("完成义务证据-{index}"),
            })
            .collect::<Vec<_>>();

        assert!(coverage.iter().all(
            |item| crate::services::coverage::evidence_exists_in_rewrite(
                item,
                std::slice::from_ref(&rewrite)
            )
        ));
        assert!(crate::services::coverage::coverage_gate_passes(
            &parsed_plan,
            &coverage,
            &[],
        ));
        assert!(crate::services::coverage::validate_state_updates(
            &parsed_plan,
            &parsed_plan.obligations[6].planned_state_updates,
        )
        .is_empty());
    }
}
