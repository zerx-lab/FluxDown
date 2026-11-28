/** 会话导出的格式（FluxCloud `GET ai/sessions/{id}/export?format=`）。 */
export type ExportFormat = "markdown" | "json";

/** 下载文件的 MIME 由格式决定，不信上游给的值。 */
const MIME: Record<ExportFormat, string> = {
  markdown: "text/markdown;charset=utf-8",
  json: "application/json;charset=utf-8",
};

/** 文件名里不能出现路径分隔符、控制字符与 Windows 保留字符。 */
function safeFileName(name: string): string {
  const cleaned = Array.from(name, (char) => (char.charCodeAt(0) < 0x20 || /[\\/:*?"<>|]/.test(char) ? "_" : char))
    .join("")
    .trim()
    .slice(0, 120);
  return cleaned === "" ? "conversation" : cleaned;
}

/** 把正文存成文件：临时 `<a download>` 点一下（只在浏览器里调用）。 */
export function downloadText(fileName: string, format: ExportFormat, content: string): void {
  const url = URL.createObjectURL(new Blob([content], { type: MIME[format] }));
  const link = document.createElement("a");
  link.href = url;
  link.download = safeFileName(fileName);
  link.rel = "noopener";
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
