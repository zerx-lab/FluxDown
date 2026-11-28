import { useEffect, useRef, useState, type ReactNode } from "react";
import { Spinner } from "@/components/account/dashboard/ui";

interface Props {
  /** 第一次点击前的内容（通常是图标或「删除」）。 */
  children: ReactNode;
  /** 第二次点击确认时显示的文案（危险色）。 */
  confirmLabel: string;
  /** 无障碍名称 / 悬停提示（第一次点击前）。 */
  label: string;
  busy?: boolean;
  disabled?: boolean;
  className: string;
  onConfirm: () => void;
  /** 确认态的样式（缺省沿用 `className`）：图标按钮要换成放得下文案的形状。 */
  confirmClassName?: string;
}

/**
 * 危险操作的「再点一次确认」按钮：第一次点击进入确认态（危险色），第二次才执行；
 * 鼠标移开或焦点离开即取消，免得确认态留在界面上被误点。确认态的按钮自动取得焦点，键盘用户直接回车确认。
 */
export default function ConfirmButton({ children, confirmLabel, label, busy = false, disabled = false, className, confirmClassName, onConfirm }: Props) {
  const [confirming, setConfirming] = useState(false);
  const button = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (confirming) button.current?.focus();
  }, [confirming]);

  if (confirming) {
    return (
      <button
        ref={button}
        type="button"
        disabled={busy || disabled}
        onClick={() => {
          setConfirming(false);
          onConfirm();
        }}
        onBlur={() => setConfirming(false)}
        onMouseLeave={() => setConfirming(false)}
        className={`${confirmClassName ?? className} border-danger bg-danger/10 text-danger disabled:opacity-60`}
      >
        {confirmLabel}
      </button>
    );
  }
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={busy || disabled}
      onClick={() => setConfirming(true)}
      className={`${className} disabled:opacity-50`}
    >
      {busy ? <Spinner size={13} /> : children}
    </button>
  );
}
