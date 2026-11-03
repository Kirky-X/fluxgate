// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

import { forwardRef, type ButtonHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "default" | "destructive" | "outline" | "ghost";
  size?: "default" | "sm";
}

const variantClass: Record<NonNullable<ButtonProps["variant"]>, string> = {
  default: "bg-primary-solid text-bg-base hover:bg-primary-hover",
  destructive: "bg-danger-solid text-bg-base hover:opacity-90",
  outline:
    "border border-text-muted text-text-primary hover:bg-bg-hover",
  ghost: "text-text-secondary hover:bg-bg-hover hover:text-text-primary",
};

const sizeClass: Record<NonNullable<ButtonProps["size"]>, string> = {
  default: "h-11 px-4 py-2",
  sm: "h-8 px-3 text-sm",
};

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant = "default", size = "default", ...props }, ref) => (
    <button
      ref={ref}
      className={cn(
        "inline-flex items-center justify-center rounded-md text-sm font-medium transition-colors duration-150 disabled:pointer-events-none disabled:opacity-50",
        variantClass[variant],
        sizeClass[size],
        className,
      )}
      {...props}
    />
  ),
);
Button.displayName = "Button";
