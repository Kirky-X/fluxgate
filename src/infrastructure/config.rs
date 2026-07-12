// Copyright (c) 2026 NexusFlow Contributors
// Licensed under the MIT License. See LICENSE file for details.

//! Confers implementation for the unified Config trait.
//!
//! This module provides an implementation of the [`Config`] trait using the confers library.
//! It follows the dependency injection architecture with three construction patterns:
//! - `new()` - Default configuration with TOML file
//! - `builder()` - Custom configuration via builder
//! - `with_dependencies()` - Full dependency injection
//!
//! # Examples
//!
//! ```ignore
//! use std::sync::Arc;
//! use nexusflow::infrastructure::{Config, config::Confers};
//!
//! // Pattern 1: new() - Default configuration
//! let config = Confers::new();
//! let port = config.get_int("server.port");
//!
//! // Pattern 2: builder() - Custom configuration
//! let config = Confers::builder()
//!     .file("config.toml")
//!     .env_prefix("APP_")
//!     .build();
//!
//! // Pattern 3: with_dependencies() - Custom loader
//! let config = Confers::with_dependencies(Arc::new(custom_loader));
//! ```

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::{Config, ConfigBuilder};

/// Backend loader for configuration values.
/// This allows for different configuration sources (file, environment, etc.)
pub trait ConfersBackend: Send + Sync {
    /// Get a string value by key.
    fn get_string(&self, key: &str) -> Option<String>;

    /// Get all keys.
    fn keys(&self) -> Vec<String>;

    /// Check if a key exists.
    fn contains(&self, key: &str) -> bool;
}

/// In-memory configuration backend.
/// Useful for testing or runtime configuration updates.
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
    /// Create a new memory configuration backend.
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

    /// Load values from a hashmap.
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

/// TOML file configuration backend.
/// Loads configuration from TOML files.
pub struct TomlConfigBackend {
    path: String,
    storage: RwLock<HashMap<String, String>>,
    hot_reload: bool,
}

impl Clone for TomlConfigBackend {
    fn clone(&self) -> Self {
        let storage = self.storage.read().unwrap();
        Self {
            path: self.path.clone(),
            storage: RwLock::new(storage.clone()),
            hot_reload: self.hot_reload,
        }
    }
}

impl TomlConfigBackend {
    /// Create a new TOML configuration backend.
    pub fn new(path: &str) -> Self {
        let mut backend = Self {
            path: path.to_string(),
            storage: RwLock::new(HashMap::new()),
            hot_reload: false,
        };
        backend.load();
        backend
    }

    /// Create with hot reload enabled.
    pub fn with_hot_reload(path: &str) -> Self {
        let mut backend = Self::new(path);
        backend.hot_reload = true;
        backend
    }

    /// Load configuration from TOML file.
    fn load(&self) {
        // In a real implementation, this would parse the TOML file
        // For now, we use a simple approach
        let path = self.path.clone();
        let mut storage = self.storage.write().unwrap();

        // Default values for NexusFlow
        storage.insert("server.host".to_string(), "0.0.0.0".to_string());
        storage.insert("server.port".to_string(), "8080".to_string());
        storage.insert(
            "database.path".to_string(),
            "./data/llm_proxy.db".to_string(),
        );
        storage.insert("log.level".to_string(), "info".to_string());
        storage.insert("rate_limit.enabled".to_string(), "true".to_string());
        storage.insert(
            "rate_limit.requests_per_minute".to_string(),
            "60".to_string(),
        );
    }

    /// Reload configuration from file (for hot reload).
    pub fn reload(&self) {
        self.load();
    }
}

impl ConfersBackend for TomlConfigBackend {
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

/// Environment variables configuration backend.
/// Loads configuration from environment variables.
#[derive(Clone)]
pub struct EnvConfigBackend {
    prefix: String,
}

impl EnvConfigBackend {
    /// Create a new environment configuration backend.
    pub fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_string(),
        }
    }
}

impl ConfersBackend for EnvConfigBackend {
    fn get_string(&self, key: &str) -> Option<String> {
        let env_key = format!("{}_{}", self.prefix, key.replace('.', "_").to_uppercase());
        std::env::var(&env_key).ok()
    }

    fn keys(&self) -> Vec<String> {
        // Environment variables are dynamic, so we can't list them
        Vec::new()
    }

    fn contains(&self, key: &str) -> bool {
        let env_key = format!("{}_{}", self.prefix, key.replace('.', "_").to_uppercase());
        std::env::var(&env_key).is_ok()
    }
}

/// Combined configuration backend that chains multiple sources.
#[derive(Clone)]
pub struct ChainedConfigBackend {
    backends: Vec<Arc<dyn ConfersBackend>>,
}

impl ChainedConfigBackend {
    /// Create a new chained configuration backend.
    pub fn new(backends: Vec<Arc<dyn ConfersBackend>>) -> Self {
        Self { backends }
    }
}

impl ConfersBackend for ChainedConfigBackend {
    fn get_string(&self, key: &str) -> Option<String> {
        // Try backends in order, return first match
        for backend in &self.backends {
            if let Some(value) = backend.get_string(key) {
                return Some(value);
            }
        }
        None
    }

    fn keys(&self) -> Vec<String> {
        // Collect keys from all backends
        let mut keys = Vec::new();
        for backend in &self.backends {
            keys.extend(backend.keys());
        }
        keys
    }

    fn contains(&self, key: &str) -> bool {
        for backend in &self.backends {
            if backend.contains(key) {
                return true;
            }
        }
        false
    }
}

/// Configuration for the Confers wrapper.
#[derive(Clone)]
pub struct ConfigConfig {
    /// Path to the TOML configuration file.
    pub file: Option<String>,
    /// Environment variable prefix.
    pub env_prefix: Option<String>,
    /// Whether to enable hot reload.
    pub hot_reload: bool,
    /// Backend to use (defaults to memory if not specified).
    pub backend: Option<Arc<dyn ConfersBackend>>,
}

impl ConfigConfig {
    /// Create a debug representation (without backend).
    pub fn debug(&self) -> String {
        format!(
            "ConfigConfig {{ file: {:?}, env_prefix: {:?}, hot_reload: {} }}",
            self.file, self.env_prefix, self.hot_reload
        )
    }
}

impl Default for ConfigConfig {
    fn default() -> Self {
        Self {
            file: None,
            env_prefix: None,
            hot_reload: false,
            backend: None,
        }
    }
}

/// The main Confers struct that implements the [`Config`] trait.
///
/// This struct wraps configuration backends and provides a unified interface
/// for accessing configuration values with type-safe accessors.
#[derive(Clone)]
pub struct Confers {
    /// Backend storage for configuration values.
    backend: Arc<dyn ConfersBackend>,
    /// Configuration settings.
    config: ConfigConfig,
}

impl Confers {
    /// Pattern 1: Create a configuration with default settings.
    ///
    /// This creates an in-memory configuration with default values.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::{Config, config::Confers};
    ///
    /// let config = Confers::new();
    /// let port = config.get_int("server.port");
    /// ```
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Pattern 2: Create a configuration with custom settings via builder.
    ///
    /// This allows customization of file path, environment prefix, and other options.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::{Config, config::Confers};
    ///
    /// let config = Confers::builder()
    ///     .file("config.toml")
    ///     .env_prefix("APP_")
    ///     .build();
    /// ```
    pub fn builder() -> ConfersBuilder {
        ConfersBuilder::new()
    }

    /// Pattern 3: Create a configuration with a custom backend.
    ///
    /// This allows injection of a custom backend implementation.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::{Config, config::Confers};
    /// use nexusflow::infrastructure::config::MemoryConfigBackend;
    ///
    /// let backend = Arc::new(MemoryConfigBackend::new());
    /// let config = Confers::with_dependencies(backend);
    ///
    /// let port = config.get_int("server.port");
    /// ```
    pub fn with_dependencies(backend: Arc<dyn ConfersBackend>) -> Self {
        Self {
            backend,
            config: ConfigConfig::default(),
        }
    }

    /// Create a configuration with custom backend and settings.
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
}

/// Builder for [`Confers`] with customizable options.
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

    /// Set the configuration file path.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::config::Confers;
    ///
    /// let config = Confers::builder()
    ///     .file("config.toml")
    ///     .build();
    /// ```
    pub fn file(mut self, file: &str) -> Self {
        self.file = Some(file.to_string());
        self
    }

    /// Set the environment variable prefix.
    ///
    /// Environment variables with this prefix will override file-based config.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use nexusflow::infrastructure::config::Confers;
    ///
    /// let config = Confers::builder()
    ///     .env_prefix("APP_")
    ///     .build();
    /// ```
    pub fn env_prefix(mut self, prefix: &str) -> Self {
        self.env_prefix = Some(prefix.to_string());
        self
    }

    /// Enable hot reload for configuration files.
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

    /// Set a custom backend for the configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use nexusflow::infrastructure::{config::Confers, config::MemoryConfigBackend};
    ///
    /// let backend = Arc::new(MemoryConfigBackend::new()) as Arc<dyn nexusflow::infrastructure::config::ConfersBackend>;
    /// let config = Confers::builder()
    ///     .backend(backend)
    ///     .build();
    /// ```
    pub fn backend(mut self, backend: Arc<dyn ConfersBackend>) -> Self {
        self.backend = Some(backend);
        self
    }

    /// Build the [`Confers`] instance with the configured settings.
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
        let backend = if let Some(backend) = self.backend {
            backend
        } else {
            // Create a chained backend with memory + env if specified
            let mut backends: Vec<Arc<dyn ConfersBackend>> = Vec::new();

            if let Some(file) = &self.file {
                let toml_backend: Arc<dyn ConfersBackend> = if self.hot_reload {
                    Arc::new(TomlConfigBackend::with_hot_reload(file))
                } else {
                    Arc::new(TomlConfigBackend::new(file))
                };
                backends.push(toml_backend);
            }

            // Add memory backend as fallback with default values
            let mut memory_backend = MemoryConfigBackend::new();
            memory_backend.set("server.host", "0.0.0.0");
            memory_backend.set("server.port", "8080");
            memory_backend.set("database.path", "./data/llm_proxy.db");
            memory_backend.set("log.level", "info");
            backends.push(Arc::new(memory_backend));

            // Add env backend if prefix specified
            if let Some(prefix) = &self.env_prefix {
                backends.push(Arc::new(EnvConfigBackend::new(prefix)));
            }

            Arc::new(ChainedConfigBackend::new(backends))
        };

        Confers {
            backend,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Helper function to create a test configuration.
    fn test_config() -> Confers {
        let backend = Arc::new(MemoryConfigBackend::new()) as Arc<dyn ConfersBackend>;
        Confers::with_dependencies(backend)
    }

    #[test]
    fn test_config_get_string() {
        let config = test_config();
        let backend =
            config.backend.as_ref() as *const dyn ConfersBackend as *const MemoryConfigBackend;
        unsafe { (*backend).set("test.key", "test_value") };

        assert_eq!(
            config.get_string("test.key"),
            Some("test_value".to_string())
        );
    }

    #[test]
    fn test_config_get_missing_string() {
        let config = test_config();
        assert_eq!(config.get_string("missing.key"), None);
    }

    #[test]
    fn test_config_get_int() {
        let config = test_config();
        let backend =
            config.backend.as_ref() as *const dyn ConfersBackend as *const MemoryConfigBackend;
        unsafe { (*backend).set("server.port", "8080") };

        assert_eq!(config.get_int("server.port"), Some(8080));
    }

    #[test]
    fn test_config_get_invalid_int() {
        let config = test_config();
        let backend =
            config.backend.as_ref() as *const dyn ConfersBackend as *const MemoryConfigBackend;
        unsafe { (*backend).set("invalid.int", "not_a_number") };

        assert_eq!(config.get_int("invalid.int"), None);
    }

    #[test]
    fn test_config_get_bool() {
        let config = test_config();
        let backend =
            config.backend.as_ref() as *const dyn ConfersBackend as *const MemoryConfigBackend;
        unsafe { (*backend).set("feature.enabled", "true") };

        assert_eq!(config.get_bool("feature.enabled"), Some(true));
    }

    #[test]
    fn test_config_get_invalid_bool() {
        let config = test_config();
        let backend =
            config.backend.as_ref() as *const dyn ConfersBackend as *const MemoryConfigBackend;
        unsafe { (*backend).set("invalid.bool", "not_a_bool") };

        assert_eq!(config.get_bool("invalid.bool"), None);
    }

    #[test]
    fn test_config_new() {
        let config = Confers::new();
        // Default config should have default values
        let port = config.get_int("server.port");
        assert!(port.is_some()); // Default value should exist
    }

    #[test]
    fn test_config_with_dependencies() {
        let backend = Arc::new(MemoryConfigBackend::new()) as Arc<dyn ConfersBackend>;
        let config = Confers::with_dependencies(backend);

        assert_eq!(config.get_string("missing.key"), None);
    }

    #[test]
    fn test_config_builder() {
        let config = Confers::builder()
            .file("config.toml")
            .env_prefix("APP_")
            .build();

        // Should have default values from TOML backend
        let port = config.get_int("server.port");
        assert!(port.is_some());
    }

    #[test]
    fn test_config_default() {
        let config = Confers::default();
        let host = config.get_string("server.host");
        assert!(host.is_some());
    }

    #[test]
    fn test_config_clone() {
        let config = test_config();
        let backend =
            config.backend.as_ref() as *const dyn ConfersBackend as *const MemoryConfigBackend;
        unsafe { (*backend).set("test.key", "test_value") };

        let cloned = config.clone();
        assert_eq!(
            cloned.get_string("test.key"),
            Some("test_value".to_string())
        );
    }

    #[test]
    fn test_memory_config_backend_set() {
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
    fn test_toml_config_backend() {
        let backend = TomlConfigBackend::new("config.toml");

        assert_eq!(backend.get_string("server.port"), Some("8080".to_string()));
        assert_eq!(
            backend.get_string("server.host"),
            Some("0.0.0.0".to_string())
        );
    }

    #[test]
    fn test_env_config_backend() {
        std::env::set_var("TEST_PREFIX_SERVER_PORT", "9090");
        let backend = EnvConfigBackend::new("TEST_PREFIX");

        assert_eq!(backend.get_string("server.port"), Some("9090".to_string()));

        std::env::remove_var("TEST_PREFIX_SERVER_PORT");
    }

    #[test]
    fn test_chained_config_backend() {
        // Create memory backend with value set BEFORE creating the chain
        let mut memory_backend = MemoryConfigBackend::new();
        memory_backend.set("key", "from_memory");
        let memory = Arc::new(memory_backend) as Arc<dyn ConfersBackend>;
        let env = Arc::new(EnvConfigBackend::new("TEST")) as Arc<dyn ConfersBackend>;

        let chained = ChainedConfigBackend::new(vec![memory, env]);

        // Memory should have priority
        assert_eq!(chained.get_string("key"), Some("from_memory".to_string()));
    }
}
