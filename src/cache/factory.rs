use crate::cache::manager::CacheManager;
use crate::cache::redis_cache_manager::{RedisCacheConfig, RedisCacheManager};
use crate::error::{Error, Result};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Cache driver type
pub enum CacheDriver {
    Redis,
    Memory,
    None,
}

impl From<&str> for CacheDriver {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "redis" => CacheDriver::Redis,
            "memory" => CacheDriver::Memory,
            _ => CacheDriver::None,
        }
    }
}

/// Factory for creating cache managers
pub struct CacheFactory;

impl CacheFactory {
    /// Create a new cache manager based on the specified driver
    pub async fn create(
        driver: CacheDriver,
        connection_string: &str,
        prefix: Option<&str>,
    ) -> Result<Arc<Mutex<Box<dyn CacheManager + Send>>>> {
        match driver {
            CacheDriver::Redis => {
                let cache_manager = RedisCacheManager::new(RedisCacheConfig::default()).await?;
                Ok(Arc::new(Mutex::new(Box::new(cache_manager))))
            },
            CacheDriver::Memory => {
                Err(Error::CacheError("Memory cache not implemented yet".to_string()))
            },
            CacheDriver::None => {
                Err(Error::CacheError("No cache driver specified".to_string()))
            }
        }
    }

    /// Create a Redis cache manager
    pub async fn create_redis(
        connection_string: &str,
        prefix: Option<&str>,
    ) -> Result<Arc<Mutex<Box<dyn CacheManager + Send>>>> {
        Self::create(CacheDriver::Redis, connection_string, prefix).await
    }
}