#!/usr/bin/env python3
# Copyright (c) 2026 Kirky.X🌠
# SPDX-License-Identifier: MIT
"""文档链接与锚点校验(docs-consistency-r1 T002)。

扫描 docs/*.md 与 README.md / README_EN.md:
  1. 相对链接目标文件存在(仓内路径);
  2. 同文件锚点(#heading)可在该文件的标题集合中命中(GitHub slug 规则:
     小写、空格转连字符、去除标点;中文保留);
  3. 跨文件相对链接(x.md#anchor / x.md)目标存在。
外部绝对 URL(http(s)://)与 mailto 不校验(离线契约)。
退出码:0=零死链;1=存在死链(逐条列出);2=没能检查(输入缺失)。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DOCS = REPO / "docs"
TARGETS = [REPO / "README.md", REPO / "README_EN.md", *sorted(DOCS.glob("*.md"))]

LINK_RE = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
HEADING_RE = re.compile(r"^(#{1,6})\s+(.+?)\s*#*\s*$", re.MULTILINE)


def github_slug(heading: str) -> str:
    """GitHub 锚点规则(经线上实证 github.com/Kirky-X/fluxweir 的
    user-content-* id 核对):小写;剥离标点与 emoji 基字符,但保留变体
    选择符 U+FE00-FE0F(`⚙️ 配置概览` → `️-配置概览`);每个空格各转
    一个连字符,连续空格不折叠(`retry · resilience` → `retry--resilience`)。"""
    text = heading.strip().lower()
    text = re.sub(r"[^\w\s\u4e00-\u9fff\ufe00-\ufe0f-]", "", text, flags=re.UNICODE)
    return text.replace(" ", "-")


def headings_of(content: str) -> set[str]:
    return {github_slug(m.group(2)) for m in HEADING_RE.finditer(content)}


def check() -> int:
    if not TARGETS:
        print("no markdown targets found", file=sys.stderr)
        return 2
    # 预读全部目标(跨文件锚点校验需要)
    contents: dict[Path, str] = {}
    for t in TARGETS:
        if t.is_file():
            contents[t] = t.read_text(encoding="utf-8")

    dead: list[str] = []
    for path, content in contents.items():
        anchors = headings_of(content)
        for m in LINK_RE.finditer(content):
            target = m.group(1)
            if target.startswith(("http://", "https://", "mailto:", "data:")):
                continue
            # 去掉纯片段(同文件锚点)
            file_part, _, frag = target.partition("#")
            base = path.parent if not file_part else (path.parent / file_part)
            if file_part:
                resolved = base.resolve()
                if not resolved.exists():
                    dead.append(f"{path.relative_to(REPO)}: 链接目标不存在 -> {target}")
                    continue
                if resolved.is_file() and resolved.suffix == ".md" and frag:
                    peer = headings_of(resolved.read_text(encoding="utf-8"))
                    if frag not in peer:
                        dead.append(
                            f"{path.relative_to(REPO)}: 跨文件锚点缺失 -> {target}"
                        )
            elif frag and frag not in anchors:
                dead.append(f"{path.relative_to(REPO)}: 同文件锚点缺失 -> #{frag}")

    if dead:
        print(f"死链 {len(dead)} 处:")
        for d in dead:
            print(f"  {d}")
        return 1
    print(f"链接校验通过:{len(contents)} 个文件零死链")
    return 0


if __name__ == "__main__":
    sys.exit(check())
