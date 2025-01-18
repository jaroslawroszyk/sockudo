use std::time::Duration;
use anyhow::Result;
use apalis::prelude::*;
use apalis::layers::ErrorHandlingLayer;
use apalis_redis::{RedisStorage, connect, RedisContext};
use tracing::{info, error};
use serde::{Serialize, Deserialize};
use crate::queue::queue::JobData;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookJob(pub JobData);

pub struct RedisQueueDriver {
    storage: RedisStorage<WebhookJob>,
    monitor: Monitor,
}

impl RedisQueueDriver {
    pub async fn new(redis_url: &str) -> Result<Self> {
        let conn = connect(redis_url).await?;
        let storage = RedisStorage::new(conn);

        Ok(Self {
            storage,
            monitor: Monitor::new(),
        })
    }

    pub async fn add_to_queue(&self, data: JobData) -> Result<()> {
        let job = WebhookJob(data);
        self.storage.clone().push(job).await?;
        Ok(())
    }

    pub async fn process_queue<F>(&mut self, worker_name: &str, execute_job: F) -> Result<()>
    where
        F: Fn(WebhookJob) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> + Send + Sync + 'static + Clone,
    {
        let worker = WorkerBuilder::new(worker_name)
            .layer(ErrorHandlingLayer::new())
            .enable_tracing()
            .rate_limit(5, Duration::from_secs(1))
            .concurrency(2)
            .data(0usize)
            .backend(self.storage.clone())
            .build_fn(move |job: WebhookJob| {
                let execute = execute_job.clone();
                async move {
                    println!("Processing webhook: {:?}", job.0);
                    execute(job).await.unwrap();
                }
            });


        Ok(())
    }

    pub async fn run(self) -> Result<()> {
        self.monitor
            .shutdown_timeout(Duration::from_secs(5))
            .run_with_signal(async {
                info!("Monitor started");
                tokio::signal::ctrl_c().await?;
                info!("Monitor starting shutdown");
                Ok(())
            })
            .await?;

        info!("Monitor shutdown complete");
        Ok(())
    }
}

// Example usage:
/*
#[tokio::main]
async fn main() -> Result<()> {
    std::env::set_var("RUST_LOG", "debug");
    tracing_subscriber::fmt::init();

    let mut driver = RedisQueueDriver::new("redis://127.0.0.1/").await?;

    driver.process_queue("webhook-worker", |job: WebhookJob| Box::pin(async move {
        println!("Processing webhook: {:?}", job.0);
        Ok(())
    })).await?;

    // Add test job
    let job = JobData::default();
    driver.add_to_queue(job).await?;

    driver.run().await?;

    Ok(())
}
*/