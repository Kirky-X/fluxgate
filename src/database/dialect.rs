// Copyright (c) 2026 Kirky.X🌠
// SPDX-License-Identifier: MIT

//! 原始 SQL 的方言适配：占位符按后端翻译 + statement 单点构造。
//!
//! sea-orm 不对 raw statement 做占位符翻译（`Statement.sql` 的契约是
//! “已带后端专属占位符”，见 sea-orm `src/database/statement.rs` 文档），
//! 故本仓统一书写的 `?` 必须在构造 statement 前翻成 postgres 的 `$n`——
//! 裸 `?` 会被 PG 解析为运算符（`syntax error at or near "ORDER"`）。

use sea_orm::{DatabaseBackend, Statement, Value};

/// 原始 SQL 语句构造（占位符统一书写 `?`，后端差异在此收口）。
pub(crate) fn raw_statement<I>(
    backend: DatabaseBackend,
    sql: impl AsRef<str>,
    values: I,
) -> Statement
where
    I: IntoIterator<Item = Value>,
{
    Statement::from_sql_and_values(backend, dialect_placeholders(backend, sql.as_ref()), values)
}

/// 占位符方言化（纯函数）：postgres 的 `?` → 位置参数 `$n`（自 1 递增），
/// 其余后端原样返回（sqlite/mysql 的占位符本就是 `?`）。
/// 词法扫描：字符串字面量（`''` 转义）、引用标识符（`""`）与 `--`/`/* */`
/// 注释内的 `?` 是普通字符，原样保留。
pub(crate) fn dialect_placeholders(backend: DatabaseBackend, sql: &str) -> String {
    if backend != DatabaseBackend::Postgres || !sql.contains('?') {
        return sql.to_string();
    }
    let mut out = String::with_capacity(sql.len() + 8);
    let mut chars = sql.chars().peekable();
    let mut index = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                out.push(c);
                while let Some(inner) = chars.next() {
                    out.push(inner);
                    if inner == c {
                        // 双写即转义（'' / ""），跳过第二个后继续扫描字面量
                        if chars.peek() == Some(&c) {
                            out.push(chars.next().expect("peek 已确认"));
                        } else {
                            break;
                        }
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                out.push('-');
                out.push(chars.next().expect("peek 已确认"));
                for inner in chars.by_ref() {
                    out.push(inner);
                    if inner == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                out.push('/');
                out.push(chars.next().expect("peek 已确认"));
                let mut prev = '\0';
                for inner in chars.by_ref() {
                    out.push(inner);
                    if prev == '*' && inner == '/' {
                        break;
                    }
                    prev = inner;
                }
            }
            '?' => {
                index += 1;
                out.push('$');
                out.push_str(&index.to_string());
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_renders_numbered_placeholders_in_order() {
        let sql = "UPDATE vault_keys SET label = ?, mask = ? WHERE id = ?";
        assert_eq!(
            dialect_placeholders(DatabaseBackend::Postgres, sql),
            "UPDATE vault_keys SET label = $1, mask = $2 WHERE id = $3"
        );
    }

    #[test]
    fn sqlite_and_mysql_keep_question_marks() {
        let sql = "SELECT id FROM vault_keys WHERE vendor_id = ? ORDER BY created_at ASC";
        for backend in [DatabaseBackend::Sqlite, DatabaseBackend::MySql] {
            assert_eq!(dialect_placeholders(backend, sql), sql, "{backend:?}");
        }
    }

    #[test]
    fn quoted_text_and_comments_are_left_untouched() {
        let sql = "SELECT '?' AS lit, \"col?\" FROM t WHERE a = ? ESCAPE ? -- 尾注 ?\n/* 块 ? 注 */ AND b = ?";
        assert_eq!(
            dialect_placeholders(DatabaseBackend::Postgres, sql),
            "SELECT '?' AS lit, \"col?\" FROM t WHERE a = $1 ESCAPE $2 -- 尾注 ?\n/* 块 ? 注 */ AND b = $3"
        );
    }

    #[test]
    fn escaped_quote_inside_literal_does_not_end_it() {
        let sql = "SELECT 'it''s ? here' WHERE x = ?";
        assert_eq!(
            dialect_placeholders(DatabaseBackend::Postgres, sql),
            "SELECT 'it''s ? here' WHERE x = $1"
        );
    }

    #[test]
    fn raw_statement_carries_backend_and_translated_sql() {
        let stmt = raw_statement(
            DatabaseBackend::Postgres,
            "SELECT id FROM vault_keys WHERE vendor_id = ?",
            vec!["openai".into()],
        );
        assert_eq!(stmt.sql, "SELECT id FROM vault_keys WHERE vendor_id = $1");
        assert_eq!(stmt.db_backend, DatabaseBackend::Postgres);
    }
}
