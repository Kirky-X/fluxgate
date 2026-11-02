// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 崩溃安全状态文件读取（magpie-absorb-r1 R-state-001，吸收自 magpie
//! `internal/lastgood` 的断电全零文件事故教训）：主文件校验失败 → 改名
//! `*.bad-<unix_ts>` 隔离（保留取证现场）→ 回退 `*.bak`；`.bak` 也坏 →
//! `Unrecoverable`（调用方 fail-closed：vault 拒绝启动空库、config 报加载
//! 错误——**绝不当空值用**）。写侧原子性（temp+rename）由既有落盘路径
//! 承担，本模块只补读侧回退与 `.bak` 生命周期。

use std::path::{Path, PathBuf};

/// 读回退结果三态。
#[derive(Debug)]
pub enum StateFileOutcome {
    /// 主文件与 `.bak` 均不存在（首装形态）——非错误，调用方自决初值。
    Missing,
    /// 主文件读取并通过校验。
    Parsed(Vec<u8>),
    /// 主文件损坏（已隔离为 `*.bad-<ts>`），从 `*.bak` 恢复的字节。
    RecoveredFromBak { bytes: Vec<u8>, bad_path: PathBuf },
    /// 主文件与 `.bak` 均不可用（或 IO 错误）。调用方必须 fail-closed。
    Unrecoverable { reason: String },
}

/// 崩溃安全读取：`validator` 判字节是否合法（magic/版本/解析任一失败即
/// 视为损坏）。`.bak` 不存在而主文件损坏时同样 `Unrecoverable`（无恢复
/// 来源）。隔离 rename 失败不阻断判定——损坏文件留在原位，错误信息点名。
pub fn read_with_recovery<F>(path: &Path, validator: F) -> StateFileOutcome
where
    F: Fn(&[u8]) -> bool,
{
    let bak = bak_sibling(path);
    match std::fs::read(path) {
        Ok(bytes) if validator(&bytes) => StateFileOutcome::Parsed(bytes),
        Ok(bytes) => {
            // 主文件可读但非法（半写/截断/全零）——隔离后尝试 .bak
            let bad = bad_sibling(path);
            let quarantined = std::fs::rename(path, &bad).is_ok();
            match std::fs::read(&bak) {
                Ok(bak_bytes) if validator(&bak_bytes) => StateFileOutcome::RecoveredFromBak {
                    bytes: bak_bytes,
                    bad_path: if quarantined { bad } else { path.to_path_buf() },
                },
                Ok(_) => StateFileOutcome::Unrecoverable {
                    reason: format!(
                        "main file invalid (quarantined={}) and .bak invalid: {}",
                        quarantined,
                        bak.display()
                    ),
                },
                Err(e) => StateFileOutcome::Unrecoverable {
                    reason: format!(
                        "main file invalid (quarantined={quarantined}), .bak unreadable ({e}): {}",
                        bak.display()
                    ),
                },
            }
            .attach_primary_len(bytes.len())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // 主文件不存在：.bak 存在且合法 → 恢复（首装后/写回中崩溃窗口）；
            // 连 .bak 也没有 → Missing，调用方自决（vault=空柜首装、config=报错）
            match std::fs::read(&bak) {
                Ok(bak_bytes) if validator(&bak_bytes) => StateFileOutcome::RecoveredFromBak {
                    bytes: bak_bytes,
                    bad_path: path.to_path_buf(),
                },
                _ => StateFileOutcome::Missing,
            }
        }
        Err(e) => StateFileOutcome::Unrecoverable {
            reason: format!("read error on {}: {e}", path.display()),
        },
    }
}

impl StateFileOutcome {
    /// 恢复来源信息补充（主文件损坏时的原始字节数，供日志）。
    fn attach_primary_len(self, len: usize) -> Self {
        match self {
            StateFileOutcome::RecoveredFromBak { bytes, bad_path } => {
                tracing::warn!(
                    bad = %bad_path.display(),
                    primary_len = len,
                    "state file recovered from .bak; primary quarantined"
                );
                StateFileOutcome::RecoveredFromBak { bytes, bad_path }
            }
            StateFileOutcome::Unrecoverable { reason } => {
                tracing::error!(primary_len = len, %reason, "state file unrecoverable");
                StateFileOutcome::Unrecoverable { reason }
            }
            other => other,
        }
    }
}

/// 成功读取后回写 `.bak`（temp+rename 原子替换；失败仅警告——备份缺失只
/// 影响下一次崩溃窗口的恢复能力，不影响本次运行）。
pub fn write_backup(path: &Path, bytes: &[u8]) {
    let bak = bak_sibling(path);
    let tmp = bak.with_extension("bak.tmp");
    if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &bak)) {
        tracing::warn!(bak = %bak.display(), error = %e, "state .bak write failed");
    }
}

fn bak_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(".bak");
    path.with_file_name(name)
}

fn bad_sibling(path: &Path) -> PathBuf {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(&format!(".bad-{ts}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn always_ok(_: &[u8]) -> bool {
        true
    }
    fn non_empty(b: &[u8]) -> bool {
        !b.is_empty() && b != b"\0\0\0\0"
    }

    fn tmp(name: &str) -> (std::path::PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (dir.path().join(name), dir)
    }

    #[test]
    fn both_valid_parses_primary() {
        let (p, _d) = tmp("a.state");
        std::fs::write(&p, b"good").unwrap();
        std::fs::write(p.with_file_name("a.state.bak"), b"old-good").unwrap();
        match read_with_recovery(&p, non_empty) {
            StateFileOutcome::Parsed(b) => assert_eq!(b, b"good"),
            other => panic!("expect Parsed, got {other:?}"),
        }
    }

    #[test]
    fn primary_bad_recovers_from_bak_and_quarantines() {
        let (p, _d) = tmp("b.state");
        std::fs::write(&p, b"\0\0\0\0").unwrap(); // 全零（断电形态）
        std::fs::write(p.with_file_name("b.state.bak"), b"rescued").unwrap();
        match read_with_recovery(&p, non_empty) {
            StateFileOutcome::RecoveredFromBak { bytes, bad_path } => {
                assert_eq!(bytes, b"rescued");
                assert!(bad_path.to_string_lossy().contains(".bad-"), "{bad_path:?}");
                assert!(bad_path.exists(), "损坏原件必须隔离保留");
                assert!(!p.exists(), "原位不留损坏文件");
            }
            other => panic!("expect Recovered, got {other:?}"),
        }
    }

    #[test]
    fn both_bad_is_unrecoverable_never_empty_ok() {
        let (p, _d) = tmp("c.state");
        std::fs::write(&p, b"\0\0\0\0").unwrap();
        std::fs::write(p.with_file_name("c.state.bak"), b"\0\0\0\0").unwrap();
        assert!(matches!(
            read_with_recovery(&p, non_empty),
            StateFileOutcome::Unrecoverable { .. }
        ));
    }

    #[test]
    fn primary_bad_without_bak_is_unrecoverable() {
        let (p, _d) = tmp("d.state");
        std::fs::write(&p, b"\0\0\0\0").unwrap();
        assert!(matches!(
            read_with_recovery(&p, non_empty),
            StateFileOutcome::Unrecoverable { .. }
        ));
    }

    #[test]
    fn missing_primary_recovers_from_bak_or_reports_missing() {
        // 主缺 + bak 在 → 恢复
        let (p, _d) = tmp("e.state");
        std::fs::write(p.with_file_name("e.state.bak"), b"rescued").unwrap();
        match read_with_recovery(&p, always_ok) {
            StateFileOutcome::RecoveredFromBak { bytes, .. } => assert_eq!(bytes, b"rescued"),
            other => panic!("expect Recovered, got {other:?}"),
        }
        // 主缺 + bak 缺 → Missing（首装形态，非错误）
        let (p2, _d2) = tmp("e2.state");
        assert!(matches!(
            read_with_recovery(&p2, always_ok),
            StateFileOutcome::Missing
        ));
    }

    #[test]
    fn write_backup_is_atomic_and_replaces() {
        let (p, _d) = tmp("f.state");
        write_backup(&p, b"v1");
        write_backup(&p, b"v2");
        assert_eq!(
            std::fs::read(p.with_file_name("f.state.bak")).unwrap(),
            b"v2",
            "后写覆盖先写（temp+rename 原子）"
        );
        assert!(!p.with_file_name("f.state.bak.bak.tmp").exists());
    }
}
