import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

export interface DialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  children: ReactNode;
}

// 最小弹层：遮罩 + 居中面板；点遮罩关闭。无动画依赖，尊重 reduced-motion 语义。
export const Dialog = ({ open, onOpenChange, title, children }: DialogProps) => {
  if (!open) return null;
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={() => onOpenChange(false)}
      role="dialog"
      aria-modal="true"
      aria-label={title}
    >
      <div
        className={cn(
          "w-full max-w-lg rounded-lg border border-text-disabled/30 bg-bg-overlay p-4 max-h-[85vh] overflow-auto",
        )}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="mb-3 text-base font-semibold text-text-primary">{title}</h2>
        {children}
      </div>
    </div>
  );
};
