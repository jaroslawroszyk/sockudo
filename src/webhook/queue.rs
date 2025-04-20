use crate::error::Result; // Assuming this defines your Result type (e.g., anyhow::Result or a custom one)
use crate::log::Log;
use crate::webhook::types::JobData; // Assuming JobData is defined and is Send + Sync + Serialize + DeserializeOwned
use async_trait::async_trait;
use redis::{RedisResult, AsyncCommands, aio::MultiplexedConnection, RedisError}; // Added RedisError
use serde::{Serialize, de::DeserializeOwned}; // Added Serialize, DeserializeOwned traits for JobData
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;



// Define JobData if not already defined (ensure it derives needed traits)
// #[derive(Debug, Clone, Serialize, Deserialize)]
// pub struct JobData {
//     // ... fields ...
// }

// Ensure JobData implements necessary traits if not already done
impl JobData where JobData: Serialize + DeserializeOwned {}


// Define a type alias for the callback for clarity and easier management
type JobProcessorFn = Box<dyn Fn(JobData) -> Result<()> + Send + Sync + 'static>;
// Define a type alias for the Arc'd callback used in Redis manager
type ArcJobProcessorFn = Arc<dyn Fn(JobData) -> Result<()> + Send + Sync + 'static>;


#[async_trait]
pub trait QueueInterface: Send + Sync {
    async fn add_to_queue(&self, queue_name: &str, data: JobData) -> Result<()>;
    // Changed callback type to accept 'static lifetime needed by Redis workers
    async fn process_queue(&self, queue_name: &str, callback: JobProcessorFn) -> Result<()>;
    async fn disconnect(&self) -> Result<()>;
}

// --- MemoryQueueManager ---
// No major logical changes, but added comments and ensured consistency.

/// Memory-based queue manager for simple deployments
pub struct MemoryQueueManager {
    // Use channels to simulate a queue in memory
    // DashMap<String, Vec<JobData>> is implicitly Send + Sync if JobData is Send
    queues: dashmap::DashMap<String, Vec<JobData>>,
    // Store Arc'd callbacks to be consistent with Redis manager and avoid potential issues if Box wasn't 'static
    processors: dashmap::DashMap<String, ArcJobProcessorFn>,
}

impl MemoryQueueManager {
    pub fn new() -> Self {
        let queues = dashmap::DashMap::new();
        let processors = dashmap::DashMap::new();

        Self { queues, processors }
    }

    /// Starts the background processing loop. Should be called once after setup.
    pub fn start_processing(&self) {
        // Clone Arcs for the background task
        let queues = self.queues.clone();
        let processors = self.processors.clone();

        Log::info("Starting memory queue processing loop...".to_string());

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));

            loop {
                interval.tick().await;

                // Iterate through queues. DashMap allows concurrent access.
                for queue_entry in queues.iter() { // Use iter() for read access
                    let queue_name = queue_entry.key().clone();

                    // Get the processor for this queue
                    if let Some(processor) = processors.get(&queue_name) {
                        // Get a mutable reference to the queue's Vec
                        if let Some(mut jobs_vec) = queues.get_mut(&queue_name) {
                            // Take all jobs from the queue for this tick
                            // Note: If a job processor is slow, it blocks others in the same queue during this tick.
                            // Consider spawning tasks per job for better isolation if needed.
                            let jobs_to_process: Vec<JobData> = jobs_vec.drain(..).collect();

                            if !jobs_to_process.is_empty() {
                                Log::info(format!("Processing {} jobs from memory queue {}", jobs_to_process.len(), queue_name));
                                // Process each job sequentially within this tick
                                for job in jobs_to_process {
                                    // Clone the Arc'd processor for the call
                                    let processor_clone = processor.clone();
                                    match processor_clone(job) { // Call the Arc'd function
                                        Ok(_) => {},
                                        Err(e) => {
                                            Log::error(format!("Error processing job from memory queue {}: {}", queue_name, e));
                                            // Potential: Add logic here to requeue the job if needed
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }
}

#[async_trait]
impl QueueInterface for MemoryQueueManager {
    async fn add_to_queue(&self, queue_name: &str, data: JobData) -> Result<()> {
        // Ensure queue Vec exists using entry API for atomicity
        self.queues.entry(queue_name.to_string()).or_default().push(data);
        Ok(())
    }

    async fn process_queue(&self, queue_name: &str, callback: JobProcessorFn) -> Result<()> {
        // Ensure the queue Vec exists (might be redundant if add_to_queue is always called first, but safe)
        self.queues.entry(queue_name.to_string()).or_default();

        // Register processor, wrapping it in Arc
        self.processors.insert(queue_name.to_string(), Arc::from(callback)); // Store as Arc
        Log::info(format!("Registered processor for memory queue: {}", queue_name));

        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        // Nothing needed for memory queue
        Ok(())
    }
}


// --- RedisQueueManager ---

/// Redis-based queue manager for production deployments
pub struct RedisQueueManager {
    redis_connection: Arc<Mutex<MultiplexedConnection>>,
    // Store Arc'd callbacks to allow cloning them into worker tasks safely
    job_processors: dashmap::DashMap<String, ArcJobProcessorFn>,
    prefix: String,
    concurrency: usize,
}

impl RedisQueueManager {
    /// Creates a new RedisQueueManager instance.
    /// Connects to Redis and returns a Result.
    pub async fn new(redis_url: &str, prefix: &str, concurrency: usize) -> Result<Self> {
        let client = redis::Client::open(redis_url)
            .map_err(|e| crate::error::Error::Config(format!("Failed to open Redis client: {}", e)))?; // Use custom error type

        let connection = client.get_multiplexed_async_connection().await
            .map_err(|e| crate::error::Error::Connection(format!("Failed to get Redis connection: {}", e)))?; // Use custom error type

        Ok(Self {
            redis_connection: Arc::new(Mutex::new(connection)),
            job_processors: dashmap::DashMap::new(),
            prefix: prefix.to_string(),
            concurrency,
        })
    }

    // Note: start_processing is effectively done within process_queue for Redis
    pub fn start_processing(&self) {
        // This method is not strictly needed for Redis as workers start in process_queue.
        // Could be used for other setup if required in the future.
    }

    async fn format_key(&self, queue_name: &str) -> String {
        format!("{}:queue:{}", self.prefix, queue_name)
    }
}

#[async_trait]
impl QueueInterface for RedisQueueManager {
    /// Adds a job to the specified Redis queue (list).
    /// Serializes the job data to JSON.
    async fn add_to_queue(&self, queue_name: &str, data: JobData) -> Result<()>
    where
        JobData: Serialize, // Ensure JobData can be serialized
    {
        let queue_key = self.format_key(queue_name).await;
        let data_json = serde_json::to_string(&data)?; // Propagate serialization error

        let mut conn = self.redis_connection.lock().await;

        // Perform RPUSH and handle potential Redis errors
        conn.rpush(&queue_key, data_json).await
            .map_err(|e| crate::error::Error::Queue(format!("Redis RPUSH failed for queue {}: {}", queue_name, e)))?; // Use custom error type

        // Log::info(format!("Added job to Redis queue: {}", queue_name)); // Optional: reduce log verbosity

        Ok(())
    }

    /// Registers a callback for a queue and starts worker tasks to process jobs.
    async fn process_queue(&self, queue_name: &str, callback: JobProcessorFn) -> Result<()>
    where
        JobData: DeserializeOwned + Send + 'static // Ensure JobData can be deserialized and sent across threads
    {
        let queue_key = self.format_key(queue_name).await;

        // Wrap the callback in an Arc to share it safely with multiple worker tasks
        let processor_arc: ArcJobProcessorFn = Arc::from(callback);

        // Store the Arc'd callback
        self.job_processors.insert(queue_name.to_string(), processor_arc.clone());
        Log::info(format!("Registered processor and starting workers for Redis queue: {}", queue_name));

        // Start worker tasks
        for i in 0..self.concurrency {
            let worker_queue_key = queue_key.clone();
            let worker_redis_conn = self.redis_connection.clone();
            let worker_processor = processor_arc.clone(); // Clone the Arc for this worker
            let worker_queue_name = queue_name.to_string(); // Clone queue name for logging

            tokio::spawn(async move {
                Log::info(format!("Starting Redis queue worker {} for queue: {}", i, worker_queue_name));

                loop {
                    let blpop_result: RedisResult<Option<(String, String)>> = { // Type hint for clarity
                        let mut conn = worker_redis_conn.lock().await;
                        // Use BLPOP with a timeout (e.g., 1 second)
                        conn.blpop(&worker_queue_key, 1.0).await
                    };

                    match blpop_result {
                        // Successfully received a job
                        Ok(Some((_key, job_data_str))) => {
                            match serde_json::from_str::<JobData>(&job_data_str) {
                                Ok(job_data) => {
                                    // Execute the job processing callback
                                    if let Err(e) = worker_processor(job_data) { // Call the Arc'd function
                                        Log::error(format!("[Worker {}] Error processing job from Redis queue {}: {}", i, worker_queue_name, e));
                                        // Potential: Add logic here for error handling (e.g., move to dead-letter queue)
                                    } else {
                                        // Log::info(format!("[Worker {}] Successfully processed job from Redis queue {}", i, worker_queue_name)); // Optional success log
                                    }
                                },
                                Err(e) => {
                                    // Failed to deserialize the job data
                                    Log::error(format!("[Worker {}] Error deserializing job data from Redis queue {}: {}. Data: '{}'", i, worker_queue_name, e, job_data_str));
                                    // Potential: Move corrupted data to a specific place?
                                }
                            }
                        },
                        // BLPOP timed out, no job available
                        Ok(None) => {
                            // Continue loop to wait again
                            continue;
                        },
                        // Redis error during BLPOP
                        Err(e) => {
                            Log::error(format!("[Worker {}] Redis BLPOP error on queue {}: {}", i, worker_queue_name, e));
                            // Avoid hammering Redis on persistent errors
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
            });
        }

        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        // No explicit disconnect needed for MultiplexedConnection,
        // it handles reconnection. Dropping the Arc will eventually close it.
        Ok(())
    }
}


// --- QueueManagerFactory ---

/// Factory for creating queue managers
pub struct QueueManagerFactory;

impl QueueManagerFactory {
    /// Creates a queue manager instance based on the specified driver.
    pub async fn create(
        driver: &str,
        redis_url: Option<&str>,
        prefix: Option<&str>,
        concurrency: Option<usize>
    ) -> Result<Box<dyn QueueInterface>> { // Return Result to propagate errors
        match driver {
            "redis" => {
                let url = redis_url.unwrap_or("redis://127.0.0.1:6379/");
                let prefix_str = prefix.unwrap_or("sockudo"); // Consider a more generic default or make it mandatory?
                let concurrency_val = concurrency.unwrap_or(5); // Default concurrency
                Log::info(format!("Creating Redis queue manager (URL: {}, Prefix: {}, Concurrency: {})", url, prefix_str, concurrency_val));
                // Use `?` to propagate potential errors from RedisQueueManager::new
                let manager = RedisQueueManager::new(url, prefix_str, concurrency_val).await?;
                // Note: Redis workers are started via process_queue, not here.
                Ok(Box::new(manager))
            },
            "memory" | _ => { // Default to memory queue manager
                Log::info("Creating Memory queue manager".to_string());
                let manager = MemoryQueueManager::new();
                // Start the single processing loop for the memory manager *after* creation.
                // The user needs to call process_queue afterwards to register processors.
                manager.start_processing(); // Start its background task here
                Ok(Box::new(manager))
            }
        }
    }
}


// --- QueueManager Wrapper ---
// Seems fine, just delegates calls.

/// General Queue Manager interface wrapper
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
    pub async fn process_queue(&self, queue_name: &str, callback: JobProcessorFn) -> Result<()> {
        self.driver.process_queue(queue_name, callback).await
    }

    /// Disconnects the underlying driver (if necessary).
    pub async fn disconnect(&self) -> Result<()> {
        self.driver.disconnect().await
    }
}

// --- Helper Error Enum (Example) ---
// You should replace this with your actual error enum/handling logic in `crate::error`
mod error {
    use thiserror::Error; // Add `thiserror` to Cargo.toml if using

    #[derive(Error, Debug)]
    pub enum Error {
        #[error("Configuration error: {0}")]
        Config(String),
        #[error("Connection error: {0}")]
        Connection(String),
        #[error("Queue operation error: {0}")]
        Queue(String),
        #[error("Serialization error: {0}")]
        Serialization(#[from] serde_json::Error),
        #[error("IO error: {0}")]
        Io(#[from] std::io::Error),
        #[error("Generic error: {0}")]
        General(String),
        // Add other specific error kinds as needed
    }

    pub type Result<T> = std::result::Result<T, Error>;
}

// --- Helper Log Struct (Example) ---
// Replace with your actual logging implementation
mod log {
    pub struct Log;
    impl Log {
        pub fn info(msg: String) { println!("INFO: {}", msg); }
        pub fn error(msg: String) { eprintln!("ERROR: {}", msg); }
        // Add other levels (warn, debug, trace)
    }
}

// --- Webhook Types (Example Placeholder) ---
mod webhook { pub mod types {
    use serde::{Deserialize, Serialize};
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct JobData { pub id: u32, pub payload: String }
}}