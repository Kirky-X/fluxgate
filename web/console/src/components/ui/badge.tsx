// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import type { HTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  variant?: "default" | "success" | "warning" | "destructive" | "outline";
}

const variantClass: Record<NonNullable<BadgeProps["variant"]>, string> = {
  default: "bg-primary-subtle text-primary-solid",
  success: "bg-success-subtle text-success-text",
  warning: "bg-warning-subtle text-warning-text",
  destructive: "bg-danger-subtle text-danger-text",
  outline: "border border-text-muted text-text-secondary",
};

export const Badge = ({ className, variant = "default", ...props }: BadgeProps) => (
  <span
    className={cn(
      "inline-flex items-center rounded-sm px-2 py-0.5 text-xs font-medium",
      variantClass[variant],
      className,
    )}
    {...props}
  />
);
