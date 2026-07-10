pub(crate) const PROTAGONIST_GRAPH_STRATEGY: &str = "protagonist_graph_v1";
pub(crate) const PROTAGONIST_RULE_PACK_VERSION: &str = "protagonist-graph-v1";
pub(crate) const LEGACY_REWRITE_STRATEGY: &str = "legacy";
pub(crate) const REWRITE_CHECK_OFF: &str = "off";
pub(crate) const REWRITE_CHECK_TAGGED: &str = "tagged";

pub(crate) const DEEP_DELTA_CATEGORIES: &[&str] = &[
    "self_perception",
    "other_reaction",
    "interaction_boundary",
    "dialogue_register",
    "social_reputation",
    "power_rationale",
    "relationship_tension",
    "action_expression",
    "humor_misunderstanding",
    "continuity_callback",
];

pub(crate) fn normalize_rewrite_strategy(value: &str) -> String {
    match value.trim() {
        LEGACY_REWRITE_STRATEGY => LEGACY_REWRITE_STRATEGY.to_string(),
        _ => PROTAGONIST_GRAPH_STRATEGY.to_string(),
    }
}

pub(crate) fn normalize_rewrite_check_mode(value: &str) -> String {
    match value.trim() {
        REWRITE_CHECK_TAGGED => REWRITE_CHECK_TAGGED.to_string(),
        _ => REWRITE_CHECK_OFF.to_string(),
    }
}

pub(crate) fn graph_strategy_name_enabled(strategy: &str) -> bool {
    normalize_rewrite_strategy(strategy) == PROTAGONIST_GRAPH_STRATEGY
}

pub(crate) fn protagonist_rule_pack() -> &'static str {
    r#"【protagonist-graph-v1 规则包】
R0_FORMAT：章节 marker、范围、顺序、标题和纯正文输出最高优先级。
R1_PLOT_ABILITY：保留原著事件功能、因果、战力、能力、人物动机和关键结果。
R2_IDENTITY_MAPPING：姓名映射和用户指定性转角色必须一致；未指定角色保持原身份与性别。
R3_PROTAGONIST_NODE_DELTA：每个主角直接出现、被提及或造成后果的节点都必须出现至少一项深层变化；姓名、代词、称谓和外貌替换不能单独算完成。
R4_MALE_INTERACTION：男性互动按女性主角身份重构社交距离、身体接触、称兄道弟、竞争、保护和旁人误会，同时保留关系功能与强度。
R5_FEMALE_RELATION：女性关系保持原关系性质；已有感情线保留确定性，普通关系可自然重构亲近、信任、照顾和交流方式，不凭空增加恋爱对象。
R6_SOCIAL_CAUSALITY：重构旁人反应、名声、身份判断、公开评价和原男性身份造成的叙事因果。
R7_CONTINUITY：跨章承诺、关系边界、称谓、社会认知和衍生状态必须承接前文。
R8_STYLE：风格补充只影响语言、节奏和表现方式，不得覆盖以上规则。
R9_CLEANUP：只保守清理广告、乱码、更新提示和无关噪音，不删除剧情、番外、后记或专有信息。

优先级：R0 → R1/R2 → R3-R7 → R8/R9。"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_strategy_is_the_default_normalization() {
        assert_eq!(normalize_rewrite_strategy(""), PROTAGONIST_GRAPH_STRATEGY);
        assert_eq!(
            normalize_rewrite_strategy("unknown"),
            PROTAGONIST_GRAPH_STRATEGY
        );
        assert_eq!(
            normalize_rewrite_strategy("legacy"),
            LEGACY_REWRITE_STRATEGY
        );
    }

    #[test]
    fn rule_pack_requires_deep_protagonist_changes() {
        let pack = protagonist_rule_pack();
        assert!(pack.contains("R3_PROTAGONIST_NODE_DELTA"));
        assert!(pack.contains("姓名、代词、称谓和外貌替换不能单独算完成"));
    }
}
