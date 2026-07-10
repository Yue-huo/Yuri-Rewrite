use crate::domain::{
    AppState, Chapter, ModelProfile, NovelSettings, RewritePlan, RewriteStateUpdate,
};
use crate::services::planning::plan_fingerprint;
use crate::{
    impact_nodes_for_chapters, load_canon_asset_content, parse_impact_graph,
    project_relevant_continuity, serialize_impact_graph, to_string, IMPACT_GRAPH_ASSET_KIND,
    PROTAGONIST_RULE_PACK_VERSION, REWRITE_CONTINUITY_ASSET_KIND,
};
use rusqlite::{params, OptionalExtension};
use tauri::State;
use uuid::Uuid;

pub(crate) struct ContractReuseContext<'a> {
    pub(crate) novel_id: &'a str,
    pub(crate) chapters: &'a [Chapter],
    pub(crate) batch_index: i64,
    pub(crate) settings: &'a NovelSettings,
    pub(crate) style_prompt: &'a str,
    pub(crate) profile: &'a ModelProfile,
    pub(crate) accumulated_state: &'a [RewriteStateUpdate],
    pub(crate) expected_run_id: &'a str,
}

pub(crate) fn load_or_create_rewrite_run_id(
    state: &State<'_, AppState>,
    novel_id: &str,
    batch_index: Option<i64>,
) -> Result<String, String> {
    let Some(batch_index) = batch_index else {
        return Ok(Uuid::new_v4().to_string());
    };
    let conn = state.conn.lock().map_err(to_string)?;
    let checkpoint = conn
        .query_row(
            "SELECT rewrite_run_id, batch_index
             FROM auto_run_checkpoints WHERE novel_id = ?1",
            params![novel_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                ))
            },
        )
        .optional()
        .map_err(to_string)?
        .ok_or_else(|| "当前一键任务缺少恢复检查点。".to_string())?;
    if checkpoint.1 != Some(batch_index) {
        return Err(format!(
            "恢复检查点批次不一致：期望 {}，实际 {:?}。",
            batch_index, checkpoint.1
        ));
    }
    if let Some(run_id) = checkpoint.0.filter(|value| !value.trim().is_empty()) {
        return Ok(run_id);
    }
    let run_id = Uuid::new_v4().to_string();
    let updated = conn
        .execute(
            "UPDATE auto_run_checkpoints
             SET rewrite_run_id = ?1, updated_at = ?2
             WHERE novel_id = ?3 AND batch_index = ?4",
            params![
                run_id,
                chrono::Utc::now().to_rfc3339(),
                novel_id,
                batch_index
            ],
        )
        .map_err(to_string)?;
    if updated != 1 {
        return Err("无法绑定当前改写运行与恢复检查点。".to_string());
    }
    Ok(run_id)
}

pub(crate) fn load_relevant_graph_context(
    state: &State<'_, AppState>,
    novel_id: &str,
    chapters: &[Chapter],
    plan: &RewritePlan,
) -> Result<(Vec<crate::domain::SourceImpactNode>, String), String> {
    let conn = state.conn.lock().map_err(to_string)?;
    let graph = load_canon_asset_content(&conn, novel_id, IMPACT_GRAPH_ASSET_KIND)?
        .map(|content| parse_impact_graph(&content))
        .unwrap_or_default();
    let nodes = impact_nodes_for_chapters(&graph, chapters);
    let continuity = load_canon_asset_content(&conn, novel_id, REWRITE_CONTINUITY_ASSET_KIND)?
        .unwrap_or_else(|| "[]".to_string());
    let continuity = project_relevant_continuity(&continuity, &nodes, Some(plan), &[]);
    Ok((nodes, continuity))
}

fn reusable_record_matches(
    status: &str,
    rule_pack_version: &str,
    fingerprint: &str,
    batch_index: Option<i64>,
    expected_fingerprint: &str,
    expected_batch_index: i64,
) -> bool {
    matches!(status, "planned" | "failed")
        && rule_pack_version == PROTAGONIST_RULE_PACK_VERSION
        && fingerprint == expected_fingerprint
        && batch_index == Some(expected_batch_index)
}

fn load_matching_contract_json(
    conn: &rusqlite::Connection,
    novel_id: &str,
    chapters: &[Chapter],
    expected_fingerprint: &str,
    expected_batch_index: i64,
    expected_run_id: &str,
) -> Result<Option<String>, String> {
    let mut contract_json: Option<String> = None;
    let mut run_id: Option<String> = None;
    for chapter in chapters {
        let record = conn
            .query_row(
                "SELECT contract_json, validation_status, rule_pack_version,
                        plan_fingerprint, batch_index, run_id
                 FROM rewrite_contracts WHERE chapter_id = ?1 AND novel_id = ?2",
                params![chapter.id, novel_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(to_string)?;
        let Some((candidate, status, version, fingerprint, batch_index, candidate_run_id)) = record
        else {
            return Ok(None);
        };
        if !reusable_record_matches(
            &status,
            &version,
            &fingerprint,
            batch_index,
            expected_fingerprint,
            expected_batch_index,
        ) {
            return Ok(None);
        }
        if candidate_run_id != expected_run_id
            || contract_json
                .as_ref()
                .is_some_and(|existing| existing != &candidate)
            || run_id
                .as_ref()
                .is_some_and(|existing| existing != &candidate_run_id)
        {
            return Ok(None);
        }
        contract_json = Some(candidate);
        run_id = Some(candidate_run_id);
    }
    Ok(contract_json)
}

pub(crate) fn load_reusable_rewrite_plan(
    state: &State<'_, AppState>,
    context: ContractReuseContext<'_>,
) -> Result<Option<RewritePlan>, String> {
    let conn = state.conn.lock().map_err(to_string)?;
    let graph = load_canon_asset_content(&conn, context.novel_id, IMPACT_GRAPH_ASSET_KIND)?
        .map(|content| parse_impact_graph(&content))
        .unwrap_or_default();
    let relevant_graph = impact_nodes_for_chapters(&graph, context.chapters);
    let relevant_graph_json = serialize_impact_graph(&relevant_graph)?;
    let stored_continuity =
        load_canon_asset_content(&conn, context.novel_id, REWRITE_CONTINUITY_ASSET_KIND)?
            .unwrap_or_else(|| "[]".to_string());
    let relevant_continuity = project_relevant_continuity(
        &stored_continuity,
        &relevant_graph,
        None,
        context.accumulated_state,
    );
    let expected_fingerprint = plan_fingerprint(
        context.chapters,
        &relevant_graph_json,
        &relevant_continuity,
        context.settings,
        context.style_prompt,
        context.profile,
    );

    load_matching_contract_json(
        &conn,
        context.novel_id,
        context.chapters,
        &expected_fingerprint,
        context.batch_index,
        context.expected_run_id,
    )?
    .map(|json| serde_json::from_str::<RewritePlan>(&json).map_err(to_string))
    .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_db;

    fn chapter(index: i64) -> Chapter {
        Chapter {
            id: format!("chapter-{index}"),
            novel_id: "novel-1".to_string(),
            index,
            title: format!("第{index}章"),
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

    #[test]
    fn reuse_requires_matching_batch_fingerprint_and_rule_pack() {
        assert!(reusable_record_matches(
            "failed",
            PROTAGONIST_RULE_PACK_VERSION,
            "fp",
            Some(3),
            "fp",
            3,
        ));
        assert!(!reusable_record_matches(
            "failed",
            PROTAGONIST_RULE_PACK_VERSION,
            "old",
            Some(3),
            "new",
            3,
        ));
        assert!(!reusable_record_matches(
            "failed",
            PROTAGONIST_RULE_PACK_VERSION,
            "fp",
            Some(2),
            "fp",
            3,
        ));
        assert!(!reusable_record_matches(
            "stale",
            PROTAGONIST_RULE_PACK_VERSION,
            "fp",
            Some(3),
            "fp",
            3,
        ));
    }

    #[test]
    fn checkpoint_contract_rows_must_share_run_contract_batch_and_fingerprint() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO novels (id, title, source_path, encoding, status, created_at)
             VALUES ('novel-1', '测试', 'a.txt', 'UTF-8', 'imported', 'now')",
            [],
        )
        .unwrap();
        for index in 1..=2 {
            conn.execute(
                "INSERT INTO chapters (
                    id, novel_id, chapter_index, title, original_text,
                    analysis_status, rewrite_status
                 ) VALUES (?1, 'novel-1', ?2, ?3, '原文', 'completed', 'pending')",
                params![format!("chapter-{index}"), index, format!("第{index}章")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO rewrite_contracts (
                    chapter_id, novel_id, run_id, batch_index, plan_fingerprint,
                    rule_pack_version, contract_json, coverage_json, validation_status,
                    obligation_total, obligation_satisfied, updated_at
                 ) VALUES (?1, 'novel-1', 'run-1', 3, 'fp',
                    ?2, '{\"plan_version\":\"protagonist-graph-v1\"}', '[]',
                    'failed', 0, 0, 'now')",
                params![format!("chapter-{index}"), PROTAGONIST_RULE_PACK_VERSION],
            )
            .unwrap();
        }
        let chapters = vec![chapter(1), chapter(2)];

        assert!(
            load_matching_contract_json(&conn, "novel-1", &chapters, "fp", 3, "run-1")
                .unwrap()
                .is_some()
        );
        assert!(
            load_matching_contract_json(&conn, "novel-1", &chapters, "other", 3, "run-1")
                .unwrap()
                .is_none()
        );
        conn.execute(
            "UPDATE rewrite_contracts SET run_id = 'run-2' WHERE chapter_id = 'chapter-2'",
            [],
        )
        .unwrap();
        assert!(
            load_matching_contract_json(&conn, "novel-1", &chapters, "fp", 3, "run-1")
                .unwrap()
                .is_none()
        );
    }
}
