use crate::domain::{AppState, Chapter, ModelProfile, NovelSettings};
use crate::{
    apply_staged_rewrites, chapters_without_staged_outputs, load_staged_chapter_ids,
    rewrite_batch_with_parallelism, save_parsed_rewrites,
};
use tauri::State;

pub(crate) struct RewriteRunContext<'a> {
    pub(crate) novel_id: &'a str,
    pub(crate) profile: &'a ModelProfile,
    pub(crate) api_key: &'a str,
    pub(crate) chapters: &'a [Chapter],
    pub(crate) canon_text: &'a str,
    pub(crate) settings: &'a NovelSettings,
    pub(crate) core_prompt: &'a str,
    pub(crate) rewrite_strategy: &'a str,
    pub(crate) rewrite_check_mode: &'a str,
    pub(crate) review_enabled: bool,
    pub(crate) review_profile: Option<&'a ModelProfile>,
    pub(crate) review_api_key: Option<&'a str>,
    pub(crate) parallelism: usize,
    pub(crate) checkpoint_batch_index: Option<i64>,
}

pub(crate) async fn rewrite_and_save(
    state: &State<'_, AppState>,
    context: RewriteRunContext<'_>,
) -> Result<(), String> {
    // A new batch/auto rewrite is always a fresh transformation of the source text.
    // Paused-task recovery is handled separately by staged plans and staged drafts, so an
    // already-saved chapter rewrite must never silently become input to a new explicit run.
    let source_chapters = chapters_from_original(context.chapters);
    let pending_chapters = if let Some(batch_index) = context.checkpoint_batch_index {
        let staged = load_staged_chapter_ids(state, context.novel_id, batch_index, "rewrite")?;
        chapters_without_staged_outputs(&source_chapters, &staged)
    } else {
        source_chapters.clone()
    };
    let rewrites = if pending_chapters.is_empty() {
        Vec::new()
    } else {
        rewrite_batch_with_parallelism(
            state,
            context.novel_id,
            context.profile,
            context.api_key,
            &source_chapters,
            &pending_chapters,
            context.canon_text,
            context.settings,
            context.core_prompt,
            context.rewrite_strategy,
            context.rewrite_check_mode,
            context.review_enabled,
            context.review_profile,
            context.review_api_key,
            context.parallelism,
            context.checkpoint_batch_index,
        )
        .await?
    };
    if let Some(batch_index) = context.checkpoint_batch_index {
        apply_staged_rewrites(state, context.novel_id, batch_index, &source_chapters)
    } else {
        save_parsed_rewrites(state, rewrites)
    }
}

fn chapters_from_original(chapters: &[Chapter]) -> Vec<Chapter> {
    chapters
        .iter()
        .cloned()
        .map(|mut chapter| {
            chapter.rewrite_text = None;
            chapter
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter_with_rewrite() -> Chapter {
        Chapter {
            id: "chapter-1".to_string(),
            novel_id: "novel-1".to_string(),
            index: 1,
            title: "第一章".to_string(),
            original_text: "原文正文".to_string(),
            analysis_json: None,
            rewrite_text: Some("上一次改写稿".to_string()),
            rewrite_edited: true,
            single_rewrite_original_available: true,
            analysis_status: "completed".to_string(),
            rewrite_status: "completed".to_string(),
            rewrite_validation_status: "passed".to_string(),
            rewrite_obligation_total: 1,
            rewrite_obligation_satisfied: 1,
        }
    }

    #[test]
    fn fresh_batch_runs_strip_saved_rewrites_from_model_source() {
        let original = chapter_with_rewrite();

        let prepared = chapters_from_original(std::slice::from_ref(&original));

        assert_eq!(prepared[0].original_text, "原文正文");
        assert!(prepared[0].rewrite_text.is_none());
        assert_eq!(original.rewrite_text.as_deref(), Some("上一次改写稿"));
    }
}
