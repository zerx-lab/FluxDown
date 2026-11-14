import { QRCodeSVG } from "qrcode.react";

/** WeChat Pay center logo: white glyph on the brand-green tile, inlined so it needs no asset. */
const WECHAT_PAY_LOGO =
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">' +
      '<rect width="48" height="48" rx="12" fill="#07C160"/>' +
      '<g fill="#ffffff" transform="translate(8 12)">' +
      '<path d="M11.6 0C5.2 0 0 4.4 0 9.9c0 3 1.6 5.7 4.2 7.6L3.1 21l3.9-2c1.3.3 2.6.5 4 .5h.6a8.4 8.4 0 0 1-.4-2.6c0-4.9 4.7-8.9 10.5-9h.6C21.2 3 16.9 0 11.6 0zM7.9 7.6a1.4 1.4 0 1 1 0-2.9 1.4 1.4 0 0 1 0 2.9zm7.4 0a1.4 1.4 0 1 1 0-2.9 1.4 1.4 0 0 1 0 2.9z"/>' +
      '<path d="M32 17.5c0-4.3-4.2-7.8-9.4-7.8s-9.4 3.5-9.4 7.8 4.2 7.8 9.4 7.8c1.1 0 2.1-.2 3.1-.5l3.1 1.7-.9-2.8c2.5-1.5 4-3.7 4-6.2zm-12.4-1.3a1.15 1.15 0 1 1 0-2.3 1.15 1.15 0 0 1 0 2.3zm6.2 0a1.15 1.15 0 1 1 0-2.3 1.15 1.15 0 0 1 0 2.3z"/>' +
      "</g></svg>",
  );

/** WeChat Pay brand tile (32px), e.g. as the leading mark of a payment option. */
export function WechatPayMark() {
  return <img src={WECHAT_PAY_LOGO} alt="" aria-hidden className="size-8 shrink-0 rounded-lg" />;
}

/** WeChat Pay pill label. */
export function WechatBadge({ label }: { label: string }) {
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full border border-[#07C160]/35 bg-[#07C160]/10 px-2.5 py-1 text-[11px] font-semibold text-[#07A653] dark:text-[#2BD67B]">
      <svg viewBox="0 0 24 24" className="size-3.5" fill="currentColor" aria-hidden="true">
        <path d="M8.7 3C4.5 3 1 5.9 1 9.5c0 2 1.1 3.8 2.9 5l-.7 2.2 2.6-1.3c.9.2 1.8.4 2.7.4h.5a5.6 5.6 0 0 1-.3-1.8c0-3.3 3.2-6 7.1-6h.5C16.3 4.9 12.9 3 8.7 3zM6.2 8.1a.95.95 0 1 1 0-1.9.95.95 0 0 1 0 1.9zm5 0a.95.95 0 1 1 0-1.9.95.95 0 0 1 0 1.9zM23 14.4c0-2.9-2.9-5.3-6.5-5.3s-6.5 2.4-6.5 5.3 2.9 5.3 6.5 5.3c.8 0 1.5-.1 2.2-.3l2.1 1.1-.6-1.8c1.7-1 2.8-2.5 2.8-4.3zm-8.6-.9a.8.8 0 1 1 0-1.6.8.8 0 0 1 0 1.6zm4.3 0a.8.8 0 1 1 0-1.6.8.8 0 0 1 0 1.6z" />
      </svg>
      {label}
    </span>
  );
}

/**
 * QR code on a white tile framed by blueprint corner ticks (scanners need the white quiet zone
 * in both themes). `wechat` embeds the WeChat Pay logo; otherwise a plain code (wallet address).
 */
export function QrTile({
  value,
  size = 192,
  wechat = false,
  title,
}: {
  value: string;
  size?: number;
  wechat?: boolean;
  title?: string;
}) {
  const logo = Math.round(size * 0.19);
  return (
    <div className="relative inline-flex rounded-[6px] bg-white p-3.5 shadow-[0_0_0_1px_var(--line-strong)]">
      <QRCodeSVG
        value={value}
        size={size}
        level={wechat ? "H" : "M"}
        title={title}
        imageSettings={wechat ? { src: WECHAT_PAY_LOGO, height: logo, width: logo, excavate: true } : undefined}
      />
      <span aria-hidden className="pointer-events-none absolute -inset-[5px]">
        <span className="absolute left-0 top-0 h-2 w-2 border-l border-t border-fg/60" />
        <span className="absolute right-0 top-0 h-2 w-2 border-r border-t border-fg/60" />
        <span className="absolute bottom-0 left-0 h-2 w-2 border-b border-l border-fg/60" />
        <span className="absolute bottom-0 right-0 h-2 w-2 border-b border-r border-fg/60" />
      </span>
    </div>
  );
}
