// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { forwardRef, type InputHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(
  ({ className, ...props }, ref) => (
    <input
      ref={ref}
      className={cn(
        "flex h-11 w-full rounded-sm border border-text-disabled/40 bg-bg-inset px-3 text-sm text-text-primary placeholder:text-text-muted disabled:cursor-not-allowed disabled:opacity-50",
        "focus:border-primary-border focus-visible:outline-none",
        className,
      )}
      {...props}
    />
  ),
);
Input.displayName = "Input";
