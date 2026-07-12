// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Confers-backed configuration implementation.
//!
//! Real integration with the `confers` library — uses
//! `confers::ConfigBuilder::<serde_json::Value>` to merge TOML file →
//! environment variables → defaults into a single `serde_json::Value`,
//! which is then flattened into dot-notation keys for the simple
//! `Config` trait accessors.
//!
//! Three construction patterns (per AGENTS.md):
//! - `new()` — default config (defaults only, no file/env)
//! - `builder()` — fluent builder with `.file()`, `.env_prefix()`, `.hot_reload()`
//! - `with_dependencies(backend)` — full DI for tests or custom backends
//!
//! # Examples
//!
//! ```ignore
//! use nexusflow::infrastructure::{Config, config::Confers};
//!
//! // Pattern 1: defaults only
//! let config = Confers::new();
//! assert_eq!(config.get_int("server.port"), Some(8080));
//!
//! // Pattern 2: load config.toml + APP_ env overrides
//! let config = Confers::builder()
//!     .file("config.toml")
//!     .env_prefix("APP_")
//!     .build();
//! ```

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::Config;

/// Backend storage trait for configuration values.
///
/// Abstraction that lets callers inject custom storage (e.g. for tests)
/// via `Confers::with_dependencies`.
pub trait ConfersBackend: Send + Sync {
    /// Get a string value by key.
    fn get_string(&self, key: &str) -> Option<String>;

    /// Get all keys.
    fn keys(&self) -> Vec<String>;

    /// Check if a key exists.
    fn contains(&self, key: &str) -> bool;
}

/// In-memory configuration backend.
///
/// Backed by `RwLock<HashMap<String, String>>` — thread-safe, no I/O.
/// Used as the default storage after confers merges all sources into a
/// flat dot-notation map.
#[derive(Default)]
pub struct MemoryConfigBackend {
    storage: RwLock<HashMap<String, String>>,
}

impl Clone for MemoryConfigBackend {
    fn clone(&self) -> Self {
        let storage = self.storage.read().unwrap();
        Self {
            storage: RwLock::new(storage.clone()),
        }
    }
}

impl MemoryConfigBackend {
    /// Create a new empty memory backend.
    pub fn new() -> Self {
        Self {
            storage: RwLock::new(HashMap::new()),
        }
    }

    /// Set a configuration value.
    pub fn set(&self, key: &str, value: &str) {
        let mut storage = self.storage.write().unwrap();
        storage.insert(key.to_string(), value.to_string());
    }

    /// Bulk load values from a hashmap (replaces existing storage).
    pub fn load(&self, values: HashMap<String, String>) {
        let mut storage = self.storage.write().unwrap();
        *storage = values;
    }
}

impl ConfersBackend for MemoryConfigBackend {
    fn get_string(&self, key: &str) -> Option<String> {
        let storage = self.storage.read().unwrap();
        storage.get(key).cloned()
    }

    fn keys(&self) -> Vec<String> {
        let storage = self.storage.read().unwrap();
        storage.keys().cloned().collect()
    }

    fn contains(&self, key: &str) -> bool {
        let storage = self.storage.read().unwrap();
        storage.contains_key(key)
    }
}

/// Configuration metadata for the [`Confers`] wrapper.
///
/// Stored alongside the backend for introspection (e.g. debug output).
/// The `backend` field is kept `None` — the actual backend lives in the
/// `Confers.backend` field.
#[derive(Clone, Default)]
pub struct ConfigConfig {
    /// Path to the TOML configuration file.
    pub file: Option<String>,
    /// Environment variable prefix.
    pub env_prefix: Option<String>,
    /// Whether hot reload was requested (sets confers `watch` flag).
    pub hot_reload: bool,
    /// Custom backend override — always `None` after `build()`.
    pub backend: Option<Arc<dyn ConfersBackend>>,
}

impl ConfigConfig {
    /// Debug representation that omits the backend (which may be opaque).
    pub fn debug(&self) -> String {
        format!(
            "ConfigConfig {{ file: {:?}, env_prefix: {:?}, hot_reload: {} }}",
            self.file, self.env_prefix, self.hot_reload
        )
    }
}

/// The main Confers wrapper that implements the [`Config`] trait.
///
/// Wraps a [`ConfersBackend`] (typically `MemoryConfigBackend` populated
/// by confers at build time) and provides type-safe accessors.
#[derive(Clone)]
pub struct Confers {
    backend: Arc<dyn ConfersBackend>,
    config: ConfigConfig,
}

impl Confers {
    /// Pattern 1: defaults-only configuration.
    ///
    /// Loads nothing from disk — just the hardcoded defaults via confers.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::{Config, config::Confers};
    ///
    /// let config = Confers::new();
    /// assert_eq!(config.get_int("server.port"), Some(8080));
    /// ```
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Pattern 2: fluent builder for custom configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::config::Confers;
    ///
    /// let config = Confers::builder()
    ///     .file("config.toml")
    ///     .env_prefix("APP_")
    ///     .build();
    /// ```
    pub fn builder() -> ConfersBuilder {
        ConfersBuilder::new()
    }

    /// Pattern 3: full dependency injection with a custom backend.
    ///
    /// Bypasses confers entirely — the caller is responsible for populating
    /// the backend. Used primarily for tests.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::{Config, config::Confers, config::MemoryConfigBackend};
    ///
    /// let mut backend = MemoryConfigBackend::new();
    /// backend.set("server.port", "9090");
    /// let config = Confers::with_dependencies(Arc::new(backend));
    /// assert_eq!(config.get_int("server.port"), Some(9090));
    /// ```
    pub fn with_dependencies(backend: Arc<dyn ConfersBackend>) -> Self {
        Self {
            backend,
            config: ConfigConfig::default(),
        }
    }

    /// Create with custom backend and metadata.
    pub fn with_config_and_dependencies(
        backend: Arc<dyn ConfersBackend>,
        file: Option<String>,
        env_prefix: Option<String>,
        hot_reload: bool,
    ) -> Self {
        Self {
            backend,
            config: ConfigConfig {
                file,
                env_prefix,
                hot_reload,
                backend: None,
            },
        }
    }

    /// Get the configuration metadata (file path, env prefix, hot_reload flag).
    ///
    /// Useful for introspection — e.g. logging which file was loaded.
    pub fn config_metadata(&self) -> &ConfigConfig {
        &self.config
    }
}

/// Builder for [`Confers`].
#[derive(Default)]
pub struct ConfersBuilder {
    file: Option<String>,
    env_prefix: Option<String>,
    hot_reload: bool,
    backend: Option<Arc<dyn ConfersBackend>>,
}

impl ConfersBuilder {
    /// Create a new builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the TOML configuration file path.
    ///
    /// The file is loaded via `confers::ConfigBuilder::file_optional`,
    /// so missing files are silently skipped (defaults still apply).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::config::Confers;
    ///
    /// let config = Confers::builder().file("config.toml").build();
    /// ```
    pub fn file(mut self, file: &str) -> Self {
        self.file = Some(file.to_string());
        self
    }

    /// Set the environment variable prefix.
    ///
    /// Env vars matching `PREFIX_KEY_PATH` (e.g. `APP_SERVER_PORT`) override
    /// file values and defaults.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::config::Confers;
    ///
    /// let config = Confers::builder().env_prefix("APP_").build();
    /// ```
    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    /// Enable confers `watch` flag for hot reload.
    ///
    /// NOTE: This sets the flag on the confers builder but does not start
    /// an active watcher — `Config::build()` returns a snapshot. Subsequent
    /// reloads require direct confers API access.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::config::Confers;
    ///
    /// let config = Confers::builder()
    ///     .file("config.toml")
    ///     .hot_reload(true)
    ///     .build();
    /// ```
    pub fn hot_reload(mut self, enabled: bool) -> Self {
        self.hot_reload = enabled;
        self
    }

    /// Provide a custom backend, bypassing confers entirely.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::config::{Confers, ConfersBackend, MemoryConfigBackend};
    ///
    /// let backend: Arc<dyn ConfersBackend> = Arc::new(MemoryConfigBackend::new());
    /// let config = Confers::builder().backend(backend).build();
    /// ```
    pub fn backend(mut self, backend: Arc<dyn ConfersBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Build the [`Confers`] instance.
    ///
    /// Merges sources in priority order: env > file > defaults.
    /// On confers build failure, falls back to defaults only with a
    /// stderr warning (Rule 12: failures must be visible).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::config::Confers;
    ///
    /// let config = Confers::builder()
    ///     .file("config.toml")
    ///     .env_prefix("APP_")
    ///     .build();
    /// ```
    pub fn build(self) -> Confers {
        // Custom backend — skip confers entirely.
        if let Some(backend) = self.backend {
            return Confers {
                backend,
                config: ConfigConfig {
                    file: self.file,
                    env_prefix: self.env_prefix,
                    hot_reload: self.hot_reload,
                    backend: None,
                },
            };
        }

        // Build merged config via confers: file → env → defaults.
        let mut builder = confers::ConfigBuilder::<serde_json::Value>::new();
        if let Some(file) = &self.file {
            builder = builder.file_optional(file);
        }
        if let Some(prefix) = &self.env_prefix {
            builder = builder.env_prefix(prefix);
        }
        builder = builder.defaults(default_config_values());
        if self.hot_reload {
            builder = builder.watch(true);
        }

        let flat: HashMap<String, String> = match builder.build() {
            Ok(value) => {
                let mut flat = HashMap::new();
                flatten_json(&value, "", &mut flat);
                flat
            }
            Err(e) => {
                // Explicit failure visibility (Rule 12): log + fall back to defaults.
                eprintln!(
                    "[nexusflow::infrastructure::config] confers build failed: {}; falling back to defaults",
                    e
                );
                default_config_flat()
            }
        };

        let memory_backend = MemoryConfigBackend::new();
        for (key, value) in flat {
            memory_backend.set(&key, &value);
        }

        Confers {
            backend: Arc::new(memory_backend),
            config: ConfigConfig {
                file: self.file,
                env_prefix: self.env_prefix,
                hot_reload: self.hot_reload,
                backend: None,
            },
        }
    }
}

impl Config for Confers {
    fn get_string(&self, key: &str) -> Option<String> {
        self.backend.get_string(key)
    }

    fn get_int(&self, key: &str) -> Option<i64> {
        self.get_string(key)?.parse().ok()
    }

    fn get_bool(&self, key: &str) -> Option<bool> {
        self.get_string(key)?.parse().ok()
    }
}

impl Default for Confers {
    fn default() -> Self {
        Self::new()
    }
}

// ===== Private helpers =====

/// Default config values as `confers::ConfigValue`s (for the confers builder).
fn default_config_values() -> HashMap<String, confers::ConfigValue> {
    let mut defaults = HashMap::new();
    defaults.insert(
        "server.host".to_string(),
        confers::ConfigValue::string("0.0.0.0"),
    );
    defaults.insert(
        "server.port".to_string(),
        confers::ConfigValue::integer(8080),
    );
    defaults.insert(
        "database.path".to_string(),
        confers::ConfigValue::string("./data/llm_proxy.db"),
    );
    defaults.insert(
        "log.level".to_string(),
        confers::ConfigValue::string("info"),
    );
    defaults.insert(
        "rate_limit.enabled".to_string(),
        confers::ConfigValue::bool(true),
    );
    defaults.insert(
        "rate_limit.requests_per_minute".to_string(),
        confers::ConfigValue::integer(60),
    );
    defaults
}

/// Defaults flattened to `String -> String` (fallback when confers build fails).
fn default_config_flat() -> HashMap<String, String> {
    let mut flat = HashMap::new();
    flat.insert("server.host".to_string(), "0.0.0.0".to_string());
    flat.insert("server.port".to_string(), "8080".to_string());
    flat.insert(
        "database.path".to_string(),
        "./data/llm_proxy.db".to_string(),
    );
    flat.insert("log.level".to_string(), "info".to_string());
    flat.insert("rate_limit.enabled".to_string(), "true".to_string());
    flat.insert(
        "rate_limit.requests_per_minute".to_string(),
        "60".to_string(),
    );
    flat
}

/// Recursively flatten a `serde_json::Value` into dot-notation `HashMap<String, String>`.
///
/// - Objects → recurse with `key.subkey` prefix
/// - Arrays → skipped (the simple `Config` trait doesn't expose array access)
/// - Primitives → stringified and stored under the current prefix
/// - Null → skipped
fn flatten_json(value: &serde_json::Value, prefix: &str, out: &mut HashMap<String, String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                let new_prefix = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{}.{}", prefix, key)
                };
                flatten_json(val, &new_prefix, out);
            }
        }
        serde_json::Value::String(s) => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string(), s.clone());
            }
        }
        serde_json::Value::Number(n) => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string(), n.to_string());
            }
        }
        serde_json::Value::Bool(b) => {
            if !prefix.is_empty() {
                out.insert(prefix.to_string(), b.to_string());
            }
        }
        serde_json::Value::Null | serde_json::Value::Array(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `Confers` with a pre-populated memory backend (no `unsafe`).
    fn config_with(pairs: &[(&str, &str)]) -> Confers {
        let backend = MemoryConfigBackend::new();
        for (k, v) in pairs {
            backend.set(k, v);
        }
        Confers::with_dependencies(Arc::new(backend))
    }

    // ===== Config trait accessors =====

    #[test]
    fn test_config_get_string() {
        let config = config_with(&[("test.key", "test_value")]);
        assert_eq!(
            config.get_string("test.key"),
            Some("test_value".to_string())
        );
    }

    #[test]
    fn test_config_get_missing_string() {
        let config = config_with(&[]);
        assert_eq!(config.get_string("missing.key"), None);
    }

    #[test]
    fn test_config_get_int() {
        let config = config_with(&[("server.port", "8080")]);
        assert_eq!(config.get_int("server.port"), Some(8080));
    }

    #[test]
    fn test_config_get_int_negative() {
        let config = config_with(&[("offset", "-42")]);
        assert_eq!(config.get_int("offset"), Some(-42));
    }

    #[test]
    fn test_config_get_invalid_int() {
        let config = config_with(&[("invalid.int", "not_a_number")]);
        assert_eq!(config.get_int("invalid.int"), None);
    }

    #[test]
    fn test_config_get_bool_true() {
        let config = config_with(&[("feature.enabled", "true")]);
        assert_eq!(config.get_bool("feature.enabled"), Some(true));
    }

    #[test]
    fn test_config_get_bool_false() {
        let config = config_with(&[("feature.disabled", "false")]);
        assert_eq!(config.get_bool("feature.disabled"), Some(false));
    }

    #[test]
    fn test_config_get_invalid_bool() {
        let config = config_with(&[("invalid.bool", "not_a_bool")]);
        assert_eq!(config.get_bool("invalid.bool"), None);
    }

    // ===== Confers::new() / default() — defaults only, no file =====

    #[test]
    fn test_config_new_has_defaults() {
        let config = Confers::new();
        assert_eq!(config.get_int("server.port"), Some(8080));
        assert_eq!(config.get_string("server.host"), Some("0.0.0.0".to_string()));
        assert_eq!(
            config.get_string("database.path"),
            Some("./data/llm_proxy.db".to_string())
        );
    }

    #[test]
    fn test_config_default_has_defaults() {
        let config = Confers::default();
        assert_eq!(config.get_int("server.port"), Some(8080));
        assert_eq!(config.get_bool("rate_limit.enabled"), Some(true));
    }

    // ===== Builder with file (real confers TOML loading) =====

    #[test]
    fn test_config_builder_loads_toml_file() {
        // config.toml exists at the project root — cargo test runs from package root.
        let config = Confers::builder().file("config.toml").build();
        // config.toml has port = 8080
        assert_eq!(config.get_int("server.port"), Some(8080));
        assert_eq!(config.get_string("server.host"), Some("0.0.0.0".to_string()));
        // TOML [database] section
        assert_eq!(
            config.get_string("database.path"),
            Some("./data/llm_proxy.db".to_string())
        );
        // TOML [rate_limit] section
        assert_eq!(config.get_bool("rate_limit.enabled"), Some(true));
    }

    #[test]
    fn test_config_builder_missing_file_falls_back_to_defaults() {
        // Non-existent file — confers file_optional skips it, defaults still apply.
        let config = Confers::builder().file("does_not_exist.toml").build();
        assert_eq!(config.get_int("server.port"), Some(8080));
    }

    #[test]
    fn test_config_builder_with_env_prefix() {
        // No actual env var asserted (env tests are flaky in parallel test runs);
        // this just verifies that env_prefix doesn't break the build path.
        let config = Confers::builder()
            .file("config.toml")
            .env_prefix("APP_")
            .build();
        assert_eq!(config.get_int("server.port"), Some(8080));
    }

    #[test]
    fn test_config_builder_hot_reload_flag() {
        // hot_reload(true) sets the confers watch flag — build should still succeed.
        let config = Confers::builder()
            .file("config.toml")
            .hot_reload(true)
            .build();
        assert_eq!(config.get_int("server.port"), Some(8080));
    }

    #[test]
    fn test_config_builder_custom_backend_overrides_confs() {
        // When a custom backend is provided, confers is bypassed entirely.
        let backend = MemoryConfigBackend::new();
        backend.set("custom.key", "custom_value");
        let config = Confers::builder()
            .backend(Arc::new(backend))
            .file("config.toml") // should be ignored
            .build();
        assert_eq!(
            config.get_string("custom.key"),
            Some("custom_value".to_string())
        );
        // server.port is NOT in the custom backend, so it should be None
        // (confers defaults were NOT loaded).
        assert_eq!(config.get_int("server.port"), None);
    }

    // ===== with_dependencies() pattern =====

    #[test]
    fn test_config_with_dependencies() {
        let backend = MemoryConfigBackend::new();
        backend.set("injected.key", "value");
        let config = Confers::with_dependencies(Arc::new(backend));
        assert_eq!(
            config.get_string("injected.key"),
            Some("value".to_string())
        );
        assert_eq!(config.get_string("missing.key"), None);
    }

    #[test]
    fn test_config_with_config_and_dependencies() {
        let backend = MemoryConfigBackend::new();
        backend.set("k", "v");
        let config = Confers::with_config_and_dependencies(
            Arc::new(backend),
            Some("config.toml".to_string()),
            Some("APP_".to_string()),
            true,
        );
        assert_eq!(config.get_string("k"), Some("v".to_string()));
        // Metadata is stored but doesn't trigger loading.
        let metadata = config.config_metadata();
        assert_eq!(metadata.file.as_deref(), Some("config.toml"));
        assert_eq!(metadata.env_prefix.as_deref(), Some("APP_"));
        assert!(metadata.hot_reload);
    }

    // ===== Clone semantics =====

    #[test]
    fn test_config_clone_preserves_values() {
        let backend = MemoryConfigBackend::new();
        backend.set("test.key", "test_value");
        let config = Confers::with_dependencies(Arc::new(backend));
        let cloned = config.clone();
        assert_eq!(
            cloned.get_string("test.key"),
            Some("test_value".to_string())
        );
    }

    // ===== MemoryConfigBackend direct tests =====

    #[test]
    fn test_memory_config_backend_set_get() {
        let backend = MemoryConfigBackend::new();
        backend.set("key1", "value1");
        backend.set("key2", "value2");
        assert_eq!(backend.get_string("key1"), Some("value1".to_string()));
        assert_eq!(backend.get_string("key2"), Some("value2".to_string()));
    }

    #[test]
    fn test_memory_config_backend_keys() {
        let backend = MemoryConfigBackend::new();
        backend.set("key1", "value1");
        backend.set("key2", "value2");
        let keys = backend.keys();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"key1".to_string()));
        assert!(keys.contains(&"key2".to_string()));
    }

    #[test]
    fn test_memory_config_backend_contains() {
        let backend = MemoryConfigBackend::new();
        backend.set("present", "x");
        assert!(backend.contains("present"));
        assert!(!backend.contains("absent"));
    }

    #[test]
    fn test_memory_config_backend_load_replaces_storage() {
        let backend = MemoryConfigBackend::new();
        backend.set("old", "old_value");
        let mut new_values = HashMap::new();
        new_values.insert("new".to_string(), "new_value".to_string());
        backend.load(new_values);
        assert_eq!(backend.get_string("old"), None);
        assert_eq!(backend.get_string("new"), Some("new_value".to_string()));
    }

    #[test]
    fn test_memory_config_backend_clone_is_independent() {
        let backend = MemoryConfigBackend::new();
        backend.set("key", "original");
        let cloned = backend.clone();
        backend.set("key", "modified");
        // Clone is a snapshot — modifications to original don't propagate.
        assert_eq!(cloned.get_string("key"), Some("original".to_string()));
        assert_eq!(backend.get_string("key"), Some("modified".to_string()));
    }

    // ===== flatten_json unit tests =====

    #[test]
    fn test_flatten_json_simple_object() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"server": {"port": 8080, "host": "0.0.0.0"}}"#).unwrap();
        let mut flat = HashMap::new();
        flatten_json(&value, "", &mut flat);
        assert_eq!(flat.get("server.port"), Some(&"8080".to_string()));
        assert_eq!(flat.get("server.host"), Some(&"0.0.0.0".to_string()));
    }

    #[test]
    fn test_flatten_json_nested_object() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"a": {"b": {"c": true, "d": 42}}}"#,
        )
        .unwrap();
        let mut flat = HashMap::new();
        flatten_json(&value, "", &mut flat);
        assert_eq!(flat.get("a.b.c"), Some(&"true".to_string()));
        assert_eq!(flat.get("a.b.d"), Some(&"42".to_string()));
    }

    #[test]
    fn test_flatten_json_skips_arrays_and_null() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"keep": "yes", "skip_arr": [1, 2], "skip_null": null}"#,
        )
        .unwrap();
        let mut flat = HashMap::new();
        flatten_json(&value, "", &mut flat);
        assert_eq!(flat.len(), 1);
        assert_eq!(flat.get("keep"), Some(&"yes".to_string()));
    }

    #[test]
    fn test_flatten_json_top_level_primitive() {
        let value: serde_json::Value = serde_json::from_str(r#""just a string""#).unwrap();
        let mut flat = HashMap::new();
        flatten_json(&value, "", &mut flat);
        // Top-level primitives with empty prefix are skipped.
        assert!(flat.is_empty());
    }
}
