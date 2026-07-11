import { describe, expect, it } from "vitest";
import { formatSystemCanonAsset } from "./canonDisplay";

describe("system canon asset display", () => {
  it("renders impact graph JSON as readable chapter cards", () => {
    const content = JSON.stringify([{
      node_id: "impact-1",
      chapter_index: 3,
      presence_kind: "direct",
      participants: ["许纸", "陈熙"],
      source_evidence: "第一行\r\n第二行",
      narrative_function: "建立关系",
      gender_mechanisms: ["原文使用男性代词"],
      thread_keys: ["许纸-陈熙关系线"],
      links: [{ type: "continues", target: "impact-2" }],
      confidence: 0.95
    }]);

    const result = formatSystemCanonAsset("主角性别影响图", content);

    expect(result).toContain("节点 1｜第 3 章｜直接出现");
    expect(result).toContain("参与者：许纸、陈熙");
    expect(result).toContain("原文证据：\n第一行\r\n第二行");
    expect(result).toContain("continues → impact-2");
    expect(result).not.toContain('"node_id"');
  });

  it("leaves editable and malformed assets unchanged", () => {
    expect(formatSystemCanonAsset("人物卡", "原始内容")).toBe("原始内容");
    expect(formatSystemCanonAsset("主角性别影响图", "not-json")).toBe("not-json");
  });
});
