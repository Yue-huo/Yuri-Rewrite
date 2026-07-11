const qualityGateMarker = "__YURI_QUALITY_GATE__:";

export function formatUserFacingJobMessage(message: string): string {
  return message.split(qualityGateMarker).join("质量门未通过：");
}
