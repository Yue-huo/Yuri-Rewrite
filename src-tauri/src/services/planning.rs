use crate::domain::{
    AppState, Chapter, ModelProfile, NovelSettings, RewritePlan, RewriteStateUpdate,
};
use crate::{
    append_ai_log, build_rewrite_plan_prompt, format_model_log_content, format_rewrite_contract,
    generate_text, impact_nodes_for_chapters, load_canon_asset_content, merge_impact_graph_nodes,
    parse_and_validate_rewrite_plan, parse_impact_graph, project_relevant_continuity,
    serialize_impact_graph, to_string, upsert_canon_asset, IMPACT_GRAPH_ASSET_KIND,
    PROTAGONIST_RULE_PACK_VERSION,
};
use chrono::Utc;
use rusqlite::params;
use sha2::{Digest, Sha256};
use tauri::State;

const SYSTEM_REWRITE_PLANNER: &str = "你是中文小说最小充分性转规划专家。只基于原文证据区分因果重构、表层适配和原样保留；覆盖所有主角节点，但绝不为中性节点制造女性化变化。保留剧情事实，不写正文，只输出合法 JSON。";
const SYSTEM_REWRITE_PLAN_REPAIR: &str = "你是改写契约 JSON 修复专家。必须修复结构、证据、一节点一义务和节点模式问题；不得把中性节点强行升级为深层重构，只输出完整合法 JSON。";

pub(crate) struct RewritePlanningContext<'a> {
    pub(crate) novel_id: &'a str,
    pub(crate) profile: &'a ModelProfile,
    pub(crate) api_key: &'a str,
    pub(crate) chapters: &'a [Chapter],
    pub(crate) settings: &'a NovelSettings,
    pub(crate) style_prompt: &'a str,
    pub(crate) accumulated_state: &'a [RewriteStateUpdate],
    pub(crate) prior_contracts: &'a [RewritePlan],
    pub(crate) run_id: &'a str,
    pub(crate) batch_index: Option<i64>,
    pub(crate) shard_label: &'a str,
}

pub(crate) async fn plan_rewrite_shard(
    state: &State<'_, AppState>,
    context: RewritePlanningContext<'_>,
) -> Result<RewritePlan, String> {
    let (graph_content, continuity_json) = {
        let conn = state.conn.lock().map_err(to_string)?;
        (
            load_canon_asset_content(&conn, context.novel_id, IMPACT_GRAPH_ASSET_KIND)?
                .unwrap_or_else(|| "[]".to_string()),
            crate::services::contracts::load_compatible_continuity_json(
                &conn,
                context.novel_id,
            )?,
        )
    };
    let graph = parse_impact_graph(&graph_content);
    let base_nodes = impact_nodes_for_chapters(&graph, context.chapters);
    let relevant_continuity = project_relevant_continuity(
        &continuity_json,
        &base_nodes,
        None,
        context.accumulated_state,
    );
    let prompt = build_rewrite_plan_prompt(
        context.chapters,
        &base_nodes,
        &graph,
        &relevant_continuity,
        context.settings,
        context.style_prompt,
        context.prior_contracts,
    );
    let output = generate_text(
        &state.client,
        Some(state.rate_limits.clone()),
        context.profile,
        context.api_key,
        SYSTEM_REWRITE_PLANNER,
        &prompt,
        true,
    )
    .await?;
    append_ai_log(
        state,
        Some(context.novel_id),
        &context.profile.id,
        "分片改写规划",
        Some(context.shard_label),
        "success",
        &format_model_log_content(&output, context.profile, Some(true)),
        output.reasoning.as_deref(),
        Some(&output.raw_response),
    )?;

    let mut plan = match parse_and_validate_rewrite_plan(&output.text, context.chapters, &base_nodes) {
        Ok(plan) => plan,
        Err(error) => {
            // Keep the user's thinking preference for the substantive first pass. Only the
            // deterministic JSON repair disables thinking so reasoning cannot consume the entire
            // completion budget before a final body is emitted.
            let repair_profile = structured_repair_profile(context.profile);
            append_ai_log(
                state,
                Some(context.novel_id),
                &context.profile.id,
                "分片改写规划解析",
                Some(context.shard_label),
                "warning",
                &error,
                output.reasoning.as_deref(),
                Some(&output.raw_response),
            )?;
            let repair_prompt = format!(
                "规划输出校验失败：{error}\n\n请依据原始规划要求修复下列输出。不得删除节点义务，不得伪造原文证据。\n\n原始规划要求：\n{prompt}\n\n待修复输出：\n{}",
                output.text
            );
            let repaired = generate_text(
                &state.client,
                Some(state.rate_limits.clone()),
                &repair_profile,
                context.api_key,
                SYSTEM_REWRITE_PLAN_REPAIR,
                &repair_prompt,
                true,
            )
            .await?;
            append_ai_log(
                state,
                Some(context.novel_id),
                &context.profile.id,
                "分片改写规划修复",
                Some(context.shard_label),
                "success",
                &format_model_log_content(&repaired, &repair_profile, Some(true)),
                repaired.reasoning.as_deref(),
                Some(&repaired.raw_response),
            )?;
            match parse_and_validate_rewrite_plan(
                &repaired.text,
                context.chapters,
                &base_nodes,
            ) {
                Ok(plan) => plan,
                Err(repair_error) => {
                    append_ai_log(
                        state,
                        Some(context.novel_id),
                        &context.profile.id,
                        "分片改写规划修复校验",
                        Some(context.shard_label),
                        "warning",
                        &repair_error,
                        repaired.reasoning.as_deref(),
                        Some(&repaired.raw_response),
                    )?;
                    let second_repair_prompt = format!(
                        "上一次修复仍未通过确定性校验：{repair_error}\n\n请只修复这个错误并返回完整 JSON。保留全部节点和义务；R3_PRESERVE 若只是保留中性原文，则 required_changes 为空；若当前稿已有过度新增，使用 R3_PRESERVE + R3_RESTORE_SOURCE，并仅写删除新增或恢复原文的 required_changes。用于删除的指令可以引用待删除坏词，但不得把坏词作为新增要求。\n\n原始规划要求：\n{prompt}\n\n上一次修复输出：\n{}",
                        repaired.text
                    );
                    let second_repaired = generate_text(
                        &state.client,
                        Some(state.rate_limits.clone()),
                        &repair_profile,
                        context.api_key,
                        SYSTEM_REWRITE_PLAN_REPAIR,
                        &second_repair_prompt,
                        true,
                    )
                    .await?;
                    append_ai_log(
                        state,
                        Some(context.novel_id),
                        &context.profile.id,
                        "分片改写规划二次修复",
                        Some(context.shard_label),
                        "success",
                        &format_model_log_content(
                            &second_repaired,
                            &repair_profile,
                            Some(true),
                        ),
                        second_repaired.reasoning.as_deref(),
                        Some(&second_repaired.raw_response),
                    )?;
                    parse_and_validate_rewrite_plan(
                        &second_repaired.text,
                        context.chapters,
                        &base_nodes,
                    )?
                }
            }
        }
    };

    normalize_planned_state_identity(&mut plan, context.settings);
    persist_plan_and_graph(
        state,
        context.novel_id,
        context.profile,
        context.chapters,
        context.settings,
        context.style_prompt,
        &continuity_json,
        context.accumulated_state,
        &graph,
        &plan,
        context.run_id,
        context.batch_index,
    )?;
    Ok(plan)
}

fn normalize_planned_state_identity(plan: &mut RewritePlan, settings: &NovelSettings) {
    let target = settings.rewritten_protagonist_name.trim();
    if target.is_empty() {
        return;
    }
    let mut sources = std::iter::once(settings.protagonist_name.as_str())
        .chain(settings.protagonist_aliases.split(|character| {
            matches!(character, '\n' | '\r' | ',' | '，' | '、' | ';' | '；')
        }))
        .map(str::trim)
        .filter(|source| !source.is_empty() && *source != target)
        .collect::<Vec<_>>();
    sources.sort_by_key(|source| std::cmp::Reverse(source.chars().count()));
    sources.dedup();
    for state in plan.planned_state_updates.iter_mut().chain(
        plan.obligations
            .iter_mut()
            .flat_map(|obligation| obligation.planned_state_updates.iter_mut()),
    ) {
        for source in &sources {
            state.value = state.value.replace(source, target);
        }
    }
}

fn structured_repair_profile(profile: &ModelProfile) -> ModelProfile {
    let mut profile = profile.clone();
    profile.thinking_mode = "off".to_string();
    profile
}

#[allow(clippy::too_many_arguments)]
fn persist_plan_and_graph(
    state: &State<'_, AppState>,
    novel_id: &str,
    profile: &ModelProfile,
    chapters: &[Chapter],
    settings: &NovelSettings,
    style_prompt: &str,
    stored_continuity_json: &str,
    accumulated_state: &[RewriteStateUpdate],
    graph: &[crate::domain::SourceImpactNode],
    plan: &RewritePlan,
    run_id: &str,
    batch_index: Option<i64>,
) -> Result<(), String> {
    let merged_graph = merge_impact_graph_nodes(graph, &plan.graph_additions);
    let graph_json = serialize_impact_graph(&merged_graph)?;
    let relevant_graph = impact_nodes_for_chapters(&merged_graph, chapters);
    let relevant_graph_json = serialize_impact_graph(&relevant_graph)?;
    let relevant_continuity_json = project_relevant_continuity(
        stored_continuity_json,
        &relevant_graph,
        None,
        accumulated_state,
    );
    let contract_json = format_rewrite_contract(plan);
    let fingerprint = plan_fingerprint(
        chapters,
        &relevant_graph_json,
        &relevant_continuity_json,
        settings,
        style_prompt,
        profile,
    );
    let now = Utc::now().to_rfc3339();
    let mut conn = state.conn.lock().map_err(to_string)?;
    let tx = conn.transaction().map_err(to_string)?;
    upsert_canon_asset(&tx, novel_id, IMPACT_GRAPH_ASSET_KIND, &graph_json, &now)
        .map_err(to_string)?;
    for chapter in chapters {
        let total = plan
            .obligations
            .iter()
            .filter(|obligation| obligation.chapter_index == chapter.index)
            .count();
        tx.execute(
            "INSERT INTO rewrite_contracts (
                chapter_id, novel_id, run_id, batch_index, plan_fingerprint,
                rule_pack_version, contract_json, coverage_json, validation_status,
                obligation_total, obligation_satisfied, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '[]', 'planned', ?8, 0, ?9)
             ON CONFLICT(chapter_id) DO UPDATE SET
                novel_id = excluded.novel_id,
                run_id = excluded.run_id,
                batch_index = excluded.batch_index,
                plan_fingerprint = excluded.plan_fingerprint,
                rule_pack_version = excluded.rule_pack_version,
                contract_json = excluded.contract_json,
                coverage_json = '[]',
                validation_status = 'planned',
                obligation_total = excluded.obligation_total,
                obligation_satisfied = 0,
                updated_at = excluded.updated_at",
            params![
                chapter.id,
                novel_id,
                run_id,
                batch_index,
                fingerprint,
                PROTAGONIST_RULE_PACK_VERSION,
                contract_json,
                total,
                now
            ],
        )
        .map_err(to_string)?;
    }
    tx.commit().map_err(to_string)
}

pub(crate) fn plan_fingerprint(
    chapters: &[Chapter],
    graph_json: &str,
    continuity_json: &str,
    settings: &NovelSettings,
    style_prompt: &str,
    profile: &ModelProfile,
) -> String {
    let source = chapters
        .iter()
        .map(|chapter| {
            format!(
                "{}\n{}\n{}\nCURRENT_DRAFT:{}",
                chapter.index,
                chapter.title,
                chapter.original_text,
                chapter.rewrite_text.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n---\n");
    let settings_text = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        settings.protagonist_name,
        settings.protagonist_aliases,
        settings.rewritten_protagonist_name,
        settings.additional_feminize_names,
        settings.bust,
        settings.body_type,
        settings.rewrite_mode,
        settings.advanced_settings,
        settings.relationship_targets,
    );
    let payload = format!(
        "{source}\n{graph_json}\n{continuity_json}\n{settings_text}\n{style_prompt}\n{}\n{}\n{}",
        PROTAGONIST_RULE_PACK_VERSION, profile.id, profile.model
    );
    format!("{:x}", Sha256::digest(payload.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter() -> Chapter {
        Chapter {
            id: "chapter-1".to_string(),
            novel_id: "novel-1".to_string(),
            index: 1,
            title: "第一章".to_string(),
            original_text: "原文".to_string(),
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

    fn settings() -> NovelSettings {
        NovelSettings {
            novel_id: "novel-1".to_string(),
            protagonist_name: "萧炎".to_string(),
            protagonist_aliases: String::new(),
            rewritten_protagonist_name: "萧妍".to_string(),
            additional_feminize_names: String::new(),
            bust: "普通".to_string(),
            body_type: "少女".to_string(),
            rewrite_mode: "strict".to_string(),
            advanced_settings: String::new(),
            relationship_targets: "[]".to_string(),
            updated_at: "now".to_string(),
        }
    }

    fn profile() -> ModelProfile {
        ModelProfile {
            id: "profile-1".to_string(),
            name: "模型".to_string(),
            provider: "openai".to_string(),
            base_url: "https://example.invalid".to_string(),
            model: "model-a".to_string(),
            temperature: 0.7,
            top_p: 1.0,
            thinking_mode: "off".to_string(),
            prompt_obfuscation_enabled: false,
            has_api_key: false,
            api_key_storage: "none".to_string(),
            updated_at: "now".to_string(),
        }
    }

    #[test]
    fn structured_repair_disables_thinking_without_mutating_saved_profile() {
        let mut saved = profile();
        saved.thinking_mode = "auto".to_string();

        let effective = structured_repair_profile(&saved);

        assert_eq!(effective.thinking_mode, "off");
        assert_eq!(effective.id, saved.id);
        assert_eq!(effective.model, saved.model);
        assert_eq!(saved.thinking_mode, "auto");
    }

    #[test]
    fn planned_state_values_use_target_identity_but_keep_stable_thread_keys() {
        let state = RewriteStateUpdate {
            thread_key: "许纸与吉尔伽美什关系线".to_string(),
            state_type: "照顾关系".to_string(),
            value: "陈熙每日为患癌的许纸送饭".to_string(),
            chapter_index: 7,
            source_obligation_ids: vec!["O-1".to_string()],
        };
        let mut plan = RewritePlan {
            plan_version: "protagonist-graph-v1".to_string(),
            graph_additions: Vec::new(),
            obligations: vec![crate::domain::RewriteObligation {
                obligation_id: "O-1".to_string(),
                node_id: "N-1".to_string(),
                chapter_index: 7,
                rule_ids: Vec::new(),
                preserve: Vec::new(),
                required_changes: Vec::new(),
                deep_delta_categories: Vec::new(),
                forbidden_regressions: Vec::new(),
                downstream_effects: Vec::new(),
                planned_state_updates: vec![state.clone()],
            }],
            planned_state_updates: vec![state],
            cross_shard_dependencies: Vec::new(),
        };
        let mut settings = settings();
        settings.protagonist_name = "许纸".to_string();
        settings.rewritten_protagonist_name = "白纸".to_string();

        normalize_planned_state_identity(&mut plan, &settings);

        assert_eq!(plan.planned_state_updates[0].thread_key, "许纸与吉尔伽美什关系线");
        assert_eq!(plan.planned_state_updates[0].value, "陈熙每日为患癌的白纸送饭");
        assert_eq!(
            plan.obligations[0].planned_state_updates[0].value,
            "陈熙每日为患癌的白纸送饭"
        );
    }

    #[test]
    fn fingerprint_changes_for_every_contract_input_class() {
        let chapter = chapter();
        let settings = settings();
        let profile = profile();
        let base = plan_fingerprint(
            std::slice::from_ref(&chapter),
            "graph-a",
            "state-a",
            &settings,
            "style-a",
            &profile,
        );
        let changed = [
            plan_fingerprint(
                std::slice::from_ref(&chapter),
                "graph-b",
                "state-a",
                &settings,
                "style-a",
                &profile,
            ),
            plan_fingerprint(
                std::slice::from_ref(&chapter),
                "graph-a",
                "state-b",
                &settings,
                "style-a",
                &profile,
            ),
            plan_fingerprint(
                std::slice::from_ref(&chapter),
                "graph-a",
                "state-a",
                &settings,
                "style-b",
                &profile,
            ),
            {
                let mut settings = settings.clone();
                settings.body_type = "高挑".to_string();
                plan_fingerprint(
                    std::slice::from_ref(&chapter),
                    "graph-a",
                    "state-a",
                    &settings,
                    "style-a",
                    &profile,
                )
            },
            {
                let mut profile = profile.clone();
                profile.model = "model-b".to_string();
                plan_fingerprint(
                    std::slice::from_ref(&chapter),
                    "graph-a",
                    "state-a",
                    &settings,
                    "style-a",
                    &profile,
                )
            },
            {
                let mut chapter = chapter.clone();
                chapter.original_text = "变更原文".to_string();
                plan_fingerprint(
                    std::slice::from_ref(&chapter),
                    "graph-a",
                    "state-a",
                    &settings,
                    "style-a",
                    &profile,
                )
            },
        ];
        assert!(changed.iter().all(|fingerprint| fingerprint != &base));
    }
}
