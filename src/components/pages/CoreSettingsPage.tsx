import { ArrowLeft, Loader2, Save } from "lucide-react";

type CoreSettingsPageProps = {
  value: string;
  strategy?: "legacy" | "protagonist_graph_v1";
  legacyBackup?: string;
  needsReview?: boolean;
  busy: boolean;
  disabled: boolean;
  onChange: (value: string) => void;
  onBack: () => void;
  onSave: () => void;
};

export function CoreSettingsPage({
  value,
  strategy = "protagonist_graph_v1",
  legacyBackup = "",
  needsReview = false,
  busy,
  disabled,
  onChange,
  onBack,
  onSave
}: CoreSettingsPageProps) {
  return (
    <div className="page-panel">
      <div className="page-heading">
        <h2>核心设定</h2>
        <div className="panel-actions">
          <button onClick={onBack}><ArrowLeft size={16} />返回</button>
          <button onClick={onSave} disabled={busy || disabled}>
            {busy ? <Loader2 className="spin" size={16} /> : <Save size={16} />}保存
          </button>
        </div>
      </div>
      <section className="settings-section core-settings-section">
        <h3>{strategy === "protagonist_graph_v1" ? "全局文风补充" : "旧版核心设定"}</h3>
        <p className="settings-note">
          {strategy === "protagonist_graph_v1"
            ? "这里只承载文风、叙述节奏、描写密度、语气、对白和情绪氛围。它位于规则包、原著事实、分片契约和连续性之后，不能覆盖结构化改写义务。"
            : "旧版核心设定会作为旧流程的全局改写要求发送给 AI。切回主角主动重构时，它仍会原样保留为备份。"}
        </p>
        {needsReview && strategy === "protagonist_graph_v1" && <p className="settings-empty-hint">旧提示词无法可靠拆分，已完整复制到全局文风。请人工删除姓名、剧情或行为规则，只保留文风偏好。</p>}
        <textarea
          className="core-settings-input"
          disabled={disabled}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          placeholder={strategy === "protagonist_graph_v1" ? "例如：保持原文轻小说风格，句子自然流畅；动作描写细腻但不过度堆砌；对白保留角色原本语气。" : "旧版全局核心提示词"}
        />
        {!value.trim() && (
          <p className="settings-empty-hint">
            当前未填写核心设定。留空也可以正常改写；如果填写，建议只写长期通用的文风和描写偏好。
          </p>
        )}
      </section>
      {strategy === "protagonist_graph_v1" && legacyBackup.trim() && (
        <section className="settings-section core-settings-section">
          <h3>旧核心设定备份（只读）</h3>
          <p className="settings-note">升级时保留的原始 core_prompt，不会注入主角主动重构流程。</p>
          <textarea className="core-settings-input" readOnly value={legacyBackup} />
        </section>
      )}
    </div>
  );
}
