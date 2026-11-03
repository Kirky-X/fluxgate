#!/usr/bin/env bash
# Copyright (c) 2026 Kirky.X🌠
# SPDX-License-Identifier: MIT

# 全仓密钥门禁 — 固定扫描集（git ls-files 减排除），账本 = 全仓发现的稳定快照。
#
# 取代 detect-secrets-hook「扫描集 = 本次暂存文件」的固有行为：那一模式下账本
# 每次被重写为当批文件的发现，随提交批次轮换（先败重写、再提即过），且多批
# 交替提交会互相挤掉已登记条目。本脚本扫描集恒定，账本只在仓库真出现新发现
# 时才落盘重写。
#
# 排除口径与原 hook args 一致：tests/、src/sdk/tests（mock 厂商凭证夹具）、
# out/（库内研究报告工具产物）、Cargo.lock、账本自身。
#
# 行为：
#   - 全仓扫描结果 ⊆ 账本 → 通过，账本零扰动（幂等，连 generated_at 都不动）；
#   - 出现新发现 → 打印清单，账本整体重写为全仓快照（人工核实过的
#     is_verified=true 按 (filename, hashed_secret) 保留），exit 1 阻断提交；
#   - 误报处置：从本输出取 hashed_secret 手工登记进账本；真密钥 → 立即作废。
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

BASELINE=.secrets.baseline
# 不依赖 bash 4 的 mapfile：macOS 自带 bash 3.2 无该内建（hook 环境仍可能
# 走 /bin/bash）。NUL 分隔读入数组，避免路径含空白时被拆词。
FILES=()
while IFS= read -r -d '' f; do
    case "$f" in
        tests/*|src/sdk/tests/*|out/*|Cargo.lock|.secrets.baseline) continue ;;
    esac
    FILES+=("$f")
done < <(git ls-files -z)

# 入口可移植化：优先 PATH 中的 detect-secrets；未装为脚本时回落
# `python3 -m detect_secrets`（pip --user 安装常不进 PATH）。
if command -v detect-secrets >/dev/null 2>&1; then
    DS=(detect-secrets)
elif python3 -c 'import detect_secrets' >/dev/null 2>&1; then
    DS=(python3 -m detect_secrets)
else
    echo "detect-secrets 未安装：pip install detect-secrets" >&2
    exit 1
fi

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
"${DS[@]}" scan "${FILES[@]}" > "$tmp"

python3 - "$BASELINE" "$tmp" <<'PY'
import json
import sys

baseline_path, scan_path = sys.argv[1], sys.argv[2]


def entries(doc):
    return {
        (f, e["type"], e["hashed_secret"]): e
        for f, es in doc.get("results", {}).items()
        for e in es
    }


baseline = json.load(open(baseline_path, encoding="utf-8"))
scan = json.load(open(scan_path, encoding="utf-8"))
old, new = entries(baseline), entries(scan)

fresh = sorted(k for k in new if k not in old)
if not fresh:
    sys.exit(0)

for filename, _, _ in fresh:
    print(f"ERROR: Potential secrets about to be committed to git repo!")
    print(f"  Location: {filename}")
    print(f"  误报 → 从账本外 scan 输出取 hashed_secret 登记；真密钥 → 立即作废换掉")

# 全仓快照整体落账本；人工核实的 is_verified=true 按 (filename, hashed_secret) 保留
for key, entry in new.items():
    prior = old.get(key)
    if prior and prior.get("is_verified"):
        entry["is_verified"] = True
baseline["results"] = scan.get("results", {})
baseline["generated_at"] = scan.get("generated_at", baseline.get("generated_at"))
with open(baseline_path, "w", encoding="utf-8") as fh:
    json.dump(baseline, fh, indent=2, ensure_ascii=False)
    fh.write("\n")
sys.exit(1)
PY
