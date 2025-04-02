use crate::cache::manager::CacheManager;
use crate::error::{Error, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use dashmap::DashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::interval;

/// Configuration for the Memory cache manager
#[derive(Clone, Debug)]
pub struct MemoryCacheConfig {
    /// Key prefix
    pub prefix: String,
    /// Response timeout
    pub response_timeout: Option<Duration>,
    /// Global TTL for entries if not specified
    pub default_ttl: Option<Duration>,
    /// Cleanup interval in milliseconds
    pub cleanup_interval_ms: u64,
}

impl Default for MemoryCacheConfig {
    fn default() -> Self {
        Self {
            prefix: "memory_cache".to_string(),
            response_timeout: Some(Duration::from_secs(5)),
            default_ttl: Some(Duration::from_secs(3600)), // 1 hour default
            cleanup_interval_ms: 1000, // Default to cleanup every second
        }
    }
}

/// Cached Entry structure
#[derive(Clone)]
struct CacheEntry {
    value: String,
    expiration: Option<Instant>,
}

/// A Memory-based implementation of the CacheManager trait
pub struct MemoryCacheManager {
    /// Shared hashmap for storing cache entries
    cache: DashMap<String, CacheEntry>,
    /// Configuration
    config: MemoryCacheConfig,
    /// Cleanup task handle
    cleanup_task: Option<JoinHandle<()>>,
    /// Shutdown channel
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl MemoryCacheManager {
    /// Creates a new Memory cache manager with configuration
    pub fn new(config: MemoryCacheConfig) -> Self {
        let cache = DashMap::new();
        let cache_instance = Self {
            cache,
            config,
            cleanup_task: None,
            shutdown_tx: None,
        };

        // Start the cleanup task in another step to avoid initialization issues
        // The start_cleanup_task method will be called immediately after construction
        cache_instance
    }

    /// Creates a new Memory cache manager with simple configuration
    pub fn with_prefix(prefix: &str) -> Self {
        let config = MemoryCacheConfig {
            prefix: prefix.to_string(),
            ..Default::default()
        };

        Self::new(config)
    }

    /// Start the periodic cleanup task
    pub fn start_cleanup_task(&mut self) {
        // Create a channel for shutdown signaling
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        // Clone the cache for the background task
        let cache = self.cache.clone();
        let interval_ms = self.config.cleanup_interval_ms;

        // Spawn a background task to periodically clean up expired entries
        let task = tokio::spawn(async move {
            let mut cleanup_interval = interval(Duration::from_millis(interval_ms));

            loop {
                tokio::select! {
                    // Wait for the interval tick or shutdown signal
                    _ = cleanup_interval.tick() => {
                        // Perform cleanup
                        let now = Instant::now();
                        
                        // Collect keys to remove
                        let expired_keys: Vec<String> = cache
                            .iter()
                            .filter_map(|entry| {
                                let key = entry.key().clone();
                                let value = entry.value();
                                
                                // Check if the entry is expired
                                if value.expiration.map_or(false, |exp| exp <= now) {
                                    Some(key)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        
                        // Remove expired keys
                        for key in expired_keys {
                            cache.remove(&key);
                        }
                    }
                    // Break the loop if shutdown signal is received
                    _ = shutdown_rx.recv() => {
                        break;
                    }
                }
            }
        });

        self.cleanup_task = Some(task);
    }

    /// Get the prefixed key
    fn prefixed_key(&self, key: &str) -> String {
        format!("{}:{}", self.config.prefix, key)
    }

    /// Clean up expired entries - this is still useful for immediate cleanups
    fn cleanup_expired_entries(&self) {
        let now = Instant::now();

        // Collect keys to remove
        let expired_keys: Vec<String> = self.cache
            .iter()
            .filter_map(|entry| {
                let key = entry.key().clone();
                let value = entry.value();

                // Check if the entry is expired
                if value.expiration.map_or(false, |exp| exp <= now) {
                    Some(key)
                } else {
                    None
                }
            })
            .collect();

        // Remove expired keys
        for key in expired_keys {
            self.cache.remove(&key);
        }
    }
}

#[async_trait]
impl CacheManager for MemoryCacheManager {
    /// Check if the given key exists in cache
    async fn has(&mut self, key: &str) -> Result<bool> {
        let prefixed_key = self.prefixed_key(key);
        let exists = self.cache.get(&prefixed_key).map_or(false, |entry| {
            entry.expiration.map_or(true, |exp| exp > Instant::now())
        });

        Ok(exists)
    }

    /// Get a key from the cache
    /// Returns None if cache does not exist or is expired
    async fn get(&mut self, key: &str) -> Result<Option<String>> {
        let prefixed_key = self.prefixed_key(key);
        let result = self.cache.get(&prefixed_key).and_then(|entry| {
            // Check if not expired
            if entry.expiration.map_or(true, |exp| exp > Instant::now()) {
                Some(entry.value.clone())
            } else {
                // Remove expired entry
                self.cache.remove(&prefixed_key);
                None
            }
        });

        Ok(result)
    }

    /// Set or overwrite the value in the cache
    async fn set(&mut self, key: &str, value: &str, ttl_seconds: u64) -> Result<()> {
        let prefixed_key = self.prefixed_key(key);

        // Determine expiration
        let expiration = if ttl_seconds > 0 {
            Some(Instant::now() + Duration::from_secs(ttl_seconds))
        } else if let Some(default_ttl) = self.config.default_ttl {
            Some(Instant::now() + default_ttl)
        } else {
            None
        };

        // Insert or update entry
        self.cache.insert(prefixed_key, CacheEntry {
            value: value.to_string(),
            expiration,
        });

        Ok(())
    }

    /// Disconnect the manager's cache and stop the cleanup task
    async fn disconnect(&self) -> Result<()> {
        // Send shutdown signal if the task is running
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.send(()).await;
        }

        // Clear the cache
        self.cache.clear();
        Ok(())
    }

    /// Check if the cache is healthy (always true for in-memory cache)
    async fn is_healthy(&self) -> Result<bool> {
        Ok(true)
    }
}

impl Drop for MemoryCacheManager {
    fn drop(&mut self) {
        // When the cache manager is dropped, attempt to send a shutdown signal
        if let Some(tx) = &self.shutdown_tx {
            let _ = tx.try_send(());
        }

        // Abort the cleanup task if it's still running
        if let Some(task) = &self.cleanup_task {
            task.abort();
        }
    }
}

/// Extension methods for MemoryCacheManager
impl MemoryCacheManager {
    /// Delete a key from the cache
    pub async fn delete(&mut self, key: &str) -> Result<bool> {
        let prefixed_key = self.prefixed_key(key);
        let existed = self.cache.remove(&prefixed_key).is_some();

        Ok(existed)
    }

    /// Get multiple keys at once
    pub async fn get_many(&mut self, keys: &[&str]) -> Result<Vec<Option<String>>> {
        let now = Instant::now();
        let results = keys.iter().map(|&key| {
            let prefixed_key = self.prefixed_key(key);
            self.cache.get(&prefixed_key)
                .and_then(|entry| {
                    // Check if not expired
                    if entry.expiration.map_or(true, |exp| exp > now) {
                        Some(entry.value.clone())
                    } else {
                        // Mark for removal
                        self.cache.remove(&prefixed_key);
                        None
                    }
                })
        }).collect();

        Ok(results)
    }

    /// Set multiple key-value pairs at once
    pub async fn set_many(&mut self, pairs: &[(&str, &str)], ttl_seconds: u64) -> Result<()> {
        // Determine expiration
        let expiration = if ttl_seconds > 0 {
            Some(Instant::now() + Duration::from_secs(ttl_seconds))
        } else if let Some(default_ttl) = self.config.default_ttl {
            Some(Instant::now() + default_ttl)
        } else {
            None
        };

        // Insert or update entries
        for (key, value) in pairs {
            let prefixed_key = self.prefixed_key(key);
            self.cache.insert(prefixed_key, CacheEntry {
                value: value.to_string(),
                expiration,
            });
        }

        Ok(())
    }

    /// Clear all keys with the current prefix
    pub async fn clear_prefix(&mut self) -> Result<usize> {
        // Prepare prefix string once
        let prefix_str = format!("{}:", self.config.prefix);

        // Count entries with the prefix
        let removed_count = self.cache
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix_str))
            .count();

        // Remove entries with the prefix
        self.cache.retain(|k, _| !k.starts_with(&prefix_str));

        Ok(removed_count)
    }

    /// Get the remaining TTL for a key in seconds
    pub async fn ttl(&mut self, key: &str) -> Result<i64> {
        let prefixed_key = self.prefixed_key(key);
        let remaining_ttl = self.cache.get(&prefixed_key)
            .and_then(|entry| entry.expiration)
            .map(|exp| {
                let now = Instant::now();
                if exp > now {
                    exp.duration_since(now).as_secs() as i64
                } else {
                    0
                }
            })
            .unwrap_or(-1);

        Ok(remaining_ttl)
    }
}

/// Update the cache manager factory to support memory cache
impl crate::cache::factory::CacheFactory {
    /// Create a new memory cache manager
    pub fn create_memory(prefix: Option<&str>) -> Result<Arc<Mutex<Box<dyn CacheManager + Send>>>> {
        let config = MemoryCacheConfig {
            prefix: prefix.unwrap_or("memory_cache").to_string(),
            ..Default::default()
        };

        let mut cache_manager = MemoryCacheManager::new(config);
        // Start the cleanup task immediately
        cache_manager.start_cleanup_task();

        Ok(Arc::new(Mutex::new(Box::new(cache_manager))))
    }
}