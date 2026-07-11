export const systemManagedCanonKinds = new Set(["主角性别影响图", "改写连续性状态"]);

type JsonRecord = Record<string, unknown>;

function asRecord(value: unknown): JsonRecord | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as JsonRecord
    : null;
}

function text(value: unknown): string {
  return typeof value === "string" ? value.trim() : "";
}

function list(value: unknown): string[] {
  return Array.isArray(value)
    ? value.map(text).filter((item) => item !== "")
    : [];
}

function formatImpactGraph(value: unknown): string | null {
  if (!Array.isArray(value)) return null;
  const presenceLabels: Record<string, string> = {
    direct: "直接出现",
    mentioned: "被提及",
    consequence: "间接影响"
  };
  const nodes = value.map(asRecord).filter((node): node is JsonRecord => node !== null);
  if (nodes.length !== value.length) return null;
  if (nodes.length === 0) return "暂无主角影响节点。";

  return nodes.map((node, index) => {
    const chapterIndex = typeof node.chapter_index === "number" ? node.chapter_index : "—";
    const presence = text(node.presence_kind);
    const lines = [
      `节点 ${index + 1}｜第 ${chapterIndex} 章｜${(presenceLabels[presence] ?? presence) || "未分类"}`,
      `ID：${text(node.node_id) || "—"}`
    ];
    const participants = list(node.participants);
    if (participants.length > 0) lines.push(`参与者：${participants.join("、")}`);
    const evidence = text(node.source_evidence);
    if (evidence) lines.push(`原文证据：\n${evidence}`);
    const narrativeFunction = text(node.narrative_function);
    if (narrativeFunction) lines.push(`叙事功能：${narrativeFunction}`);
    const mechanisms = list(node.gender_mechanisms);
    if (mechanisms.length > 0) lines.push(`性别影响机制：${mechanisms.join("；")}`);
    const before = text(node.state_before);
    const after = text(node.state_after);
    if (before || after) lines.push(`状态变化：${before || "—"} → ${after || "—"}`);
    const threadKeys = list(node.thread_keys);
    if (threadKeys.length > 0) lines.push(`关系线：${threadKeys.join("、")}`);
    if (Array.isArray(node.links) && node.links.length > 0) {
      const links = node.links.map((link) => {
        const record = asRecord(link);
        return record ? `${text(record.type) || text(record.kind) || "关联"} → ${text(record.target) || "—"}` : "";
      }).filter(Boolean);
      if (links.length > 0) lines.push(`图连接：${links.join("；")}`);
    }
    if (typeof node.confidence === "number") lines.push(`置信度：${node.confidence}`);
    return lines.join("\n");
  }).join("\n\n────────────────────────\n\n");
}

function formatContinuity(value: unknown): string | null {
  if (!Array.isArray(value)) return null;
  const states = value.map(asRecord).filter((state): state is JsonRecord => state !== null);
  if (states.length !== value.length) return null;
  if (states.length === 0) return "暂无已通过的改写连续性状态。";

  return states.map((state, index) => {
    const chapterIndex = typeof state.chapter_index === "number" ? state.chapter_index : "—";
    const lines = [
      `状态 ${index + 1}｜第 ${chapterIndex} 章｜${text(state.thread_key) || "未命名关系线"}`,
      `类型：${text(state.state_type) || "—"}`,
      `当前值：${text(state.value) || "—"}`
    ];
    const sources = list(state.source_obligation_ids);
    if (sources.length > 0) lines.push(`来源义务：${sources.join("、")}`);
    return lines.join("\n");
  }).join("\n\n────────────────────────\n\n");
}

export function formatSystemCanonAsset(kind: string, content: string): string {
  if (!systemManagedCanonKinds.has(kind)) return content;
  try {
    const value: unknown = JSON.parse(content);
    const formatted = kind === "主角性别影响图"
      ? formatImpactGraph(value)
      : kind === "改写连续性状态"
        ? formatContinuity(value)
        : null;
    return formatted ?? JSON.stringify(value, null, 2);
  } catch {
    return content;
  }
}
