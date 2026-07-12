pub(crate) const PROTAGONIST_GRAPH_STRATEGY: &str = "protagonist_graph_v1";
pub(crate) const PROTAGONIST_RULE_PACK_VERSION: &str = "protagonist-graph-v2.1";
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

pub(crate) const OBLIGATION_MODE_RULE_IDS: &[&str] = &[
    "R3_CAUSAL_TRANSFORM",
    "R3_SURFACE_ADAPT",
    "R3_PRESERVE",
    "R3_DERIVED_TRANSFORM",
];

pub(crate) fn obligation_mode_rule(rule_ids: &[String]) -> Option<&str> {
    rule_ids
        .iter()
        .map(String::as_str)
        .find(|rule| OBLIGATION_MODE_RULE_IDS.contains(rule))
}

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
    r#"【protagonist-graph-v2.1 规则包】
R0_FORMAT：章节 marker、范围、顺序、标题和纯正文输出最高优先级。
R1_PLOT_ABILITY：保留原著事件功能、因果、战力、能力、人物动机和关键结果。
R2_IDENTITY_MAPPING：姓名映射和用户指定性转角色必须一致；未指定角色保持原身份与性别。
R3_NODE_CLASSIFICATION：每个主角节点必须被覆盖，但必须且只能分类为 R3_CAUSAL_TRANSFORM、R3_SURFACE_ADAPT、R3_PRESERVE 或 R3_DERIVED_TRANSFORM；“覆盖”不等于“必须改写”。
R3_CAUSAL_TRANSFORM：只有身体差异、明确性别称谓、恋爱/婚姻、性别化社会角色、互动边界或其他有原文证据的性别因果才做最小充分深改。
R3_SURFACE_ADAPT：只处理原文明确的性别称谓、身体差异，以及场景确实涉及身体/外貌时的自然适配；允许适量且符合设定的外貌描写，但不得借外貌强加性格、母性、柔弱或价值判断。全局姓名和代词映射本身不构成表层节点变化。
R3_PRESERVE：性别中性节点保留原事件、措辞、心理、动作、关系与中性称谓，并统一执行全局姓名/代词映射；不得把姓名、代词替换写成节点 required_changes，不得为了留下变化证据而新增内容。
R3_DERIVED_TRANSFORM：只有前序已确认的性别因果确实改变本节点时才承接，必须指出依赖；不得把同线关系本身当作变化理由。
R4_MINIMAL_CAUSALITY：使用反事实判断——若仅替换主角身份后原场景仍自然成立，就不重构该场景。禁止“女性视角”本身充当因果。
R5_ANTI_STEREOTYPE：禁止凭空加入“身为女性/女人”“我一个女人”“同为女子”“枉为女性”、女性特有的细腻/慈悲/柔弱/母性、爱美购物偏好等刻板表达。外貌描写只能描述可观察特征，不得推出人格与行为。
R6_ENTITY_TERM_LOCK：偶像、榜样、英雄、巨人、巨兽、强者、造物主、学生、同伴、朋友、管理员、师父、前辈、对手、敌人、主人、孩子、家伙等中性词默认保留；未列入姓名映射的人物姓名不得删除、改名或用代词替代。
R7_RELATION_CONTINUITY：关系性质、强度、称谓、承诺和互动边界只承接有证据的变化；普通女性关系不得自动升级为闺蜜、暧昧、依赖或母女关系。
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
    fn rule_pack_requires_classification_and_minimal_causality() {
        let pack = protagonist_rule_pack();
        assert!(pack.contains("R3_NODE_CLASSIFICATION"));
        assert!(pack.contains("覆盖”不等于“必须改写"));
        assert!(pack.contains("允许适量且符合设定的外貌描写"));
        assert!(pack.contains("偶像、榜样、英雄、巨人、巨兽"));
    }
}
