import { describe, expect, it } from "vitest";
import { formatUserFacingJobMessage } from "./jobDisplay";

describe("job display", () => {
  it("hides persisted internal quality gate markers", () => {
    expect(formatUserFacingJobMessage(
      "第1-10章：__YURI_QUALITY_GATE__:第三次覆盖审查仍未通过"
    )).toBe("第1-10章：质量门未通过：第三次覆盖审查仍未通过");
  });
});
