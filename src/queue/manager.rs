// --- QueueManager Wrapper ---
// Seems fine, just delegates calls.

use crate::error::Result;

use crate::queue::QueueInterface;
use crate::queue::memory_queue_manager::MemoryQueueManager;
use crate::queue::redis_cluster_queue_manager::RedisClusterQueueManager; // Add this import
use crate::queue::redis_queue_manager::RedisQueueManager;
use crate::webhook::sender::JobProcessorFnAsync;
use crate::webhook::types::JobData;
use tracing::info;

/// General Queue Manager interface wrapper
pub struct QueueManagerFactory;

impl QueueManagerFactory {
    /// Creates a queue manager instance based on the specified driver.
    pub async fn create(
        driver: &str,
        redis_url: Option<&str>,
        prefix: Option<&str>,
        concurrency: Option<usize>,
    ) -> Result<Box<dyn QueueInterface>> {
        // Return Result to propagate errors
        match driver {
            "redis" => {
                let url = redis_url.unwrap_or("redis://127.0.0.1:6379/");
                let prefix_str = prefix.unwrap_or("sockudo"); // Consider a more generic default or make it mandatory?
                let concurrency_val = concurrency.unwrap_or(5); // Default concurrency
                info!(
                    "{}",
                    format!(
                        "Creating Redis queue manager (URL: {}, Prefix: {}, Concurrency: {})",
                        url, prefix_str, concurrency_val
                    )
                );
                // Use `?` to propagate potential errors from RedisQueueManager::new
                let manager = RedisQueueManager::new(url, prefix_str, concurrency_val).await?;
                // Note: Redis workers are started via process_queue, not here.
                Ok(Box::new(manager))
            }
            "redis-cluster" => {
                // For cluster, redis_url should contain comma-separated cluster nodes
                let nodes_str = redis_url.unwrap_or(
                    "redis://127.0.0.1:7000,redis://127.0.0.1:7001,redis://127.0.0.1:7002",
                );
                let cluster_nodes: Vec<String> =
                    nodes_str.split(',').map(|s| s.trim().to_string()).collect();
                let prefix_str = prefix.unwrap_or("sockudo");
                let concurrency_val = concurrency.unwrap_or(5);

                info!(
                    "{}",
                    format!(
                        "Creating Redis Cluster queue manager (Nodes: {:?}, Prefix: {}, Concurrency: {})",
                        cluster_nodes, prefix_str, concurrency_val
                    )
                );

                let manager =
                    RedisClusterQueueManager::new(cluster_nodes, prefix_str, concurrency_val)
                        .await?;
                Ok(Box::new(manager))
            }
            "memory" => {
                // Default to memory queue manager
                info!("{}", "Creating Memory queue manager".to_string());
                let manager = MemoryQueueManager::new();
                // Start the single processing loop for the memory manager *after* creation.
                // The user needs to call process_queue afterwards to register processors.
                manager.start_processing(); // Start its background task here
                Ok(Box::new(manager))
            }
            other => Err(crate::error::Error::Queue(format!(
                "Unsupported queue driver: {other}"
            ))),
        }
    }
}

pub struct QueueManager {
    driver: Box<dyn QueueInterface>,
}

impl QueueManager {
    /// Creates a new QueueManager wrapping a specific driver implementation.
    pub fn new(driver: Box<dyn QueueInterface>) -> Self {
        Self { driver }
    }

    /// Adds data to the specified queue via the underlying driver.
    pub async fn add_to_queue(&self, queue_name: &str, data: JobData) -> Result<()> {
        self.driver.add_to_queue(queue_name, data).await
    }

    /// Registers a processor for the specified queue and starts processing (if applicable for the driver).
    pub async fn process_queue(
        &self,
        queue_name: &str,
        callback: JobProcessorFnAsync,
    ) -> Result<()> {
        self.driver.process_queue(queue_name, callback).await
    }

    /// Disconnects the underlying driver (if necessary).
    pub async fn disconnect(&self) -> Result<()> {
        self.driver.disconnect().await
    }
}
