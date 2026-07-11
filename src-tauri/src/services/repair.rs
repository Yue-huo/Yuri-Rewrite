use crate::domain::{AppState, Chapter, ModelProfile, NovelSettings, RewritePlan};
use crate::{
    format_repair_contract, prompt_context_or_none, protagonist_rule_pack,
    revise_rewrite_shard_after_review, ParsedChapterRewrite, ReviewDecision,
};
use std::collections::HashSet;
use tauri::State;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RevisionPlan {
    Targeted(Vec<i64>),
    Full(String),
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn issue_text(issue: &crate::domain::ReviewIssue) -> String {
    let indexes = if issue.chapter_indexes.is_empty() {
        "未定位章节".to_string()
    } else {
        issue
            .chapter_indexes
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join("、")
    };
    format!("{} [{}] {}", indexes, issue.category, issue.problem)
}

pub(crate) fn plan_review_revision(shard: &[Chapter], decision: &ReviewDecision) -> RevisionPlan {
    let shard_indexes = shard
        .iter()
        .map(|chapter| chapter.index)
        .collect::<HashSet<_>>();
    let mut target_indexes = HashSet::new();
    for issue in &decision.issues {
        let combined = format!("{} {}", issue.problem, issue.required_fix);
        let category = issue.category.to_ascii_lowercase();
        let scope = issue.scope.to_ascii_lowercase();
        let crosses_chapters = scope != "chapter"
            || contains_any(
                &category,
                &[
                    "cross",
                    "boundary",
                    "continuity",
                    "missing",
                    "duplicate",
                    "order",
                ],
            )
            || contains_any(
                &combined,
                &[
                    "跨章",
                    "连续性",
                    "章节边界",
                    "章节缺失",
                    "缺少章节",
                    "章节重复",
                    "重复章节",
                    "串章",
                    "额外章节",
                    "章节顺序",
                    "空正文",
                ],
            );
        if crosses_chapters {
            return RevisionPlan::Full(format!(
                "问题涉及跨章一致性或章节结构：{}",
                issue_text(issue)
            ));
        }
        if issue.chapter_indexes.is_empty() {
            return RevisionPlan::Full(format!(
                "审查问题未提供可定位的分片索引：{}",
                issue_text(issue)
            ));
        }
        if issue
            .chapter_indexes
            .iter()
            .any(|index| !shard_indexes.contains(index))
        {
            return RevisionPlan::Full(format!(
                "审查问题包含当前分片之外的索引：{}",
                issue_text(issue)
            ));
        }
        target_indexes.extend(issue.chapter_indexes.iter().copied());
    }

    if target_indexes.is_empty() {
        return RevisionPlan::Full("审查未提供可执行的目标章节。".to_string());
    }
    if target_indexes.len() * 2 > shard.len() {
        return RevisionPlan::Full(format!(
            "目标章节 {} 个，超过当前分片 {} 章的一半。",
            target_indexes.len(),
            shard.len()
        ));
    }
    let mut ordered = shard
        .iter()
        .filter(|chapter| target_indexes.contains(&chapter.index))
        .map(|chapter| chapter.index)
        .collect::<Vec<_>>();
    ordered.dedup();
    RevisionPlan::Targeted(ordered)
}

pub(crate) fn build_repair_core_prompt(
    shard: &[Chapter],
    style_prompt: &str,
    rewrite_plan: Option<&RewritePlan>,
    decision: &ReviewDecision,
    continuity_json: &str,
) -> String {
    let Some(plan) = rewrite_plan else {
        return style_prompt.to_string();
    };
    let target_indexes = match plan_review_revision(shard, decision) {
        RevisionPlan::Targeted(indexes) => Some(indexes.into_iter().collect::<HashSet<_>>()),
        RevisionPlan::Full(_) => None,
    };
    let issue_text = decision
        .issues
        .iter()
        .map(|issue| format!("{} {}", issue.problem, issue.required_fix))
        .collect::<Vec<_>>()
        .join("\n");
    let failed_obligation_ids = plan
        .obligations
        .iter()
        .filter(|obligation| issue_text.contains(&obligation.obligation_id))
        .map(|obligation| obligation.obligation_id.clone())
        .collect::<HashSet<_>>();
    let style_prompt = if style_prompt.trim().is_empty() {
        "无".to_string()
    } else {
        crate::truncate_text(style_prompt.trim(), 2_000)
    };
    format!(
        "【修复完整性要求】\n契约中的 repair_target_obligation_ids 是本轮直接修复目标；同一目标章节的其他 obligations 是必须保留的防回归约束，重写整章时不得删除或弱化。按节点模式修复：R3_CAUSAL_TRANSFORM / R3_DERIVED_TRANSFORM 完成最小因果变化；R3_SURFACE_ADAPT 只处理身份、称谓、身体或场景相关外貌；R3_PRESERVE 必须恢复并保留中性内容，不得添加所谓深层变化。required_changes 非空时逐项核对；为空的 R3_PRESERVE 以保留证据验收。不得引入“身为女性/女人”“同为女子”“枉为女性”、母性/柔弱/细腻等刻板表达，不得替换中性词或未映射人物姓名。适量外貌描写可以保留或补充，但必须与当前身体观察、身体互动或即时反应直接相关，且不能推出人格。由“可能、可以、例如、比如、如”等词引出的内容只表示可选实现方式，不要求逐字采用或全部出现。\n\n{}\n\n【本次修复所需契约】\n{}\n\n【相关已通过连续性状态】\n{}\n\n【低优先级全局文风补充】\n{}",
        protagonist_rule_pack(),
        format_repair_contract(plan, target_indexes.as_ref(), &failed_obligation_ids),
        prompt_context_or_none(continuity_json),
        style_prompt
    )
}

pub(crate) struct ReviewRepairContext<'a> {
    pub(crate) novel_id: &'a str,
    pub(crate) profile: &'a ModelProfile,
    pub(crate) api_key: &'a str,
    pub(crate) shard: &'a [Chapter],
    pub(crate) rewrites: &'a [ParsedChapterRewrite],
    pub(crate) canon_text: &'a str,
    pub(crate) settings: &'a NovelSettings,
    pub(crate) core_prompt: &'a str,
    pub(crate) shard_context: &'a str,
    pub(crate) shard_label: &'a str,
    pub(crate) decision: &'a ReviewDecision,
    pub(crate) tagged_check: bool,
}

pub(crate) async fn repair_reviewed_shard(
    state: &State<'_, AppState>,
    context: ReviewRepairContext<'_>,
) -> Result<Vec<ParsedChapterRewrite>, String> {
    revise_rewrite_shard_after_review(
        state,
        context.novel_id,
        context.profile,
        context.api_key,
        context.shard,
        context.rewrites,
        context.canon_text,
        context.settings,
        context.core_prompt,
        context.shard_context,
        context.shard_label,
        context.decision,
        context.tagged_check,
    )
    .await
}
