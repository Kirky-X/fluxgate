## 变更描述

<!-- 简要描述本次 PR 做了什么,以及为什么 -->

## 变更类型

- [ ] feat: 新功能
- [ ] fix: Bug 修复
- [ ] refactor: 重构
- [ ] perf: 性能优化
- [ ] docs: 文档
- [ ] test: 测试
- [ ] chore: 杂项
- [ ] ci: CI/CD
- [ ] breaking: 破坏性变更

## 检查清单

- [ ] 代码通过 `cargo fmt --all -- --check`
- [ ] 代码通过 `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] 测试通过 `cargo test --workspace`（default 口径；`--all-features` 因 db-* 互斥必然失败）
- [ ] 新增功能有对应的测试覆盖
- [ ] 依赖变更未引入本地 path / git 源（自研 7 库以 crates.io 版本集成，CI 无兄弟库克隆步骤）
- [ ] 文档已更新（API 变更→docs/API_REFERENCE.md，配置变更→docs/USER_GUIDE.md，架构变更→docs/ARCHITECTURE.md）
- [ ] docs/CHANGELOG.md 已更新（如为用户可见变更）

## 相关 Issue

<!-- 关联的 issue 编号,例如 Closes #123 -->
