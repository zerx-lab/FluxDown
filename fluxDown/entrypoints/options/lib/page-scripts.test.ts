import { describe, expect, test } from "bun:test";
import { matchHosts } from "./page-scripts";

describe("matchHosts", () => {
  test("去掉协议、通配子域与路径，按声明顺序去重", () => {
    expect(
      matchHosts([
        "https://pan.baidu.com/s/*",
        "https://pan.baidu.com/share/init*",
        "*://*.lanzoub.com/*",
        "*://115.com/s/*",
      ]),
    ).toEqual(["pan.baidu.com", "lanzoub.com", "115.com"]);
  });

  test("通配主机与无法解析的规则原样保留", () => {
    expect(matchHosts(["*://*/*", "<all_urls>"])).toEqual(["*://*/*", "<all_urls>"]);
  });
});
