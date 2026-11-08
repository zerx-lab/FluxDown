/**
 * IP 字面量与 CIDR 网段的解析 / 匹配（站点配置的 IP 网段 key 用）。
 * 只认严格写法：IPv4 为四段十进制（不接受前导零 / 八进制 / 省略段），IPv6 为标准文本（可含 `::`、末尾内嵌 IPv4）。
 */

export interface IpAddr {
  family: 4 | 6;
  value: bigint;
}

export interface Cidr extends IpAddr {
  prefix: number;
}

const V4_RE = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/;
const HEX_GROUP_RE = /^[0-9a-f]{1,4}$/i;

function parseV4(text: string): bigint | null {
  const m = V4_RE.exec(text);
  if (!m) return null;
  let value = 0n;
  for (let i = 1; i <= 4; i++) {
    const part = m[i];
    if (part.length > 1 && part.startsWith("0")) return null;
    const n = Number(part);
    if (n > 255) return null;
    value = (value << 8n) | BigInt(n);
  }
  return value;
}

function parseV6(input: string): bigint | null {
  if (!input.includes(":") || !/^[0-9a-f:.]+$/i.test(input)) return null;
  let text = input;
  const lastColon = input.lastIndexOf(":");
  const tail = input.slice(lastColon + 1);
  if (tail.includes(".")) {
    const v4 = parseV4(tail);
    if (v4 === null) return null;
    text = `${input.slice(0, lastColon + 1)}${(v4 >> 16n).toString(16)}:${(v4 & 0xffffn).toString(16)}`;
  }
  const halves = text.split("::");
  if (halves.length > 2) return null;
  const split = (part: string): string[] => (part === "" ? [] : part.split(":"));
  const head = split(halves[0]);
  let groups = head;
  if (halves.length === 2) {
    const rest = split(halves[1]);
    const fill = 8 - head.length - rest.length;
    if (fill < 1) return null;
    groups = [...head, ...new Array<string>(fill).fill("0"), ...rest];
  } else if (head.length !== 8) {
    return null;
  }
  let value = 0n;
  for (const g of groups) {
    if (!HEX_GROUP_RE.test(g)) return null;
    value = (value << 16n) | BigInt(Number.parseInt(g, 16));
  }
  return value;
}

/** 解析 IP 字面量（IPv6 可带方括号，即 `URL.hostname` 的形态）；不是 IP 返回 null。 */
export function parseIp(text: string): IpAddr | null {
  const s = text.startsWith("[") && text.endsWith("]") ? text.slice(1, -1) : text;
  const v4 = parseV4(s);
  if (v4 !== null) return { family: 4, value: v4 };
  const v6 = parseV6(s);
  if (v6 !== null) return { family: 6, value: v6 };
  return null;
}

/** 解析 `地址/前缀长度`；地址部分的主机位被清零（`10.1.2.3/8` → 网络地址 `10.0.0.0`）。 */
export function parseCidr(text: string): Cidr | null {
  const slash = text.indexOf("/");
  if (slash < 0) return null;
  const lenText = text.slice(slash + 1);
  if (!/^\d{1,3}$/.test(lenText)) return null;
  const ip = parseIp(text.slice(0, slash));
  if (!ip) return null;
  const width = ip.family === 4 ? 32 : 128;
  const prefix = Number(lenText);
  if (prefix > width) return null;
  const hostBits = BigInt(width - prefix);
  return { family: ip.family, value: (ip.value >> hostBits) << hostBits, prefix };
}

/** 网段是否包含该地址（地址族不同恒为否）。 */
export function cidrContains(cidr: Cidr, ip: IpAddr): boolean {
  if (cidr.family !== ip.family) return false;
  const hostBits = BigInt((cidr.family === 4 ? 32 : 128) - cidr.prefix);
  return ip.value >> hostBits === cidr.value >> hostBits;
}

/** 规范文本：IPv4 点分十进制、IPv6 经 URL 压缩（小写），后接 `/前缀`。 */
export function formatCidr(cidr: Cidr): string {
  if (cidr.family === 4) {
    const v = cidr.value;
    const octets = [24n, 16n, 8n, 0n].map((shift) => Number((v >> shift) & 0xffn));
    return `${octets.join(".")}/${cidr.prefix}`;
  }
  const groups: string[] = [];
  for (let shift = 112n; shift >= 0n; shift -= 16n) {
    groups.push(((cidr.value >> shift) & 0xffffn).toString(16));
  }
  const host = new URL(`http://[${groups.join(":")}]`).hostname;
  return `${host.slice(1, -1)}/${cidr.prefix}`;
}

/**
 * 形如 CIDR 的输入（点分十进制样式或含两个以上冒号的 IPv6 样式 + `/` + 纯数字）：此时解析失败
 * 应判非法，不能当域名处理。地址部分不按十六进制字符集放宽，否则 `add.de/12` 这类由 a-f 组成的
 * 域名带数字路径会被误判。
 */
export function looksLikeCidr(text: string): boolean {
  return /^(?:\d+(?:\.\d+){0,3}|\[?[0-9a-f]*:[0-9a-f]*:[0-9a-f:.]*\]?)\/\d+$/i.test(text);
}
