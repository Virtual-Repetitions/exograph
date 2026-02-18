// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

#![cfg(feature = "pool")]

use std::time::Duration;

#[cfg(feature = "postgres-url")]
use deadpool_postgres::ConfigConnectImpl;
use deadpool_postgres::{
    Connect, Hook, HookError, Manager, ManagerConfig, Pool, RecyclingMethod, Runtime,
};

use tokio_postgres::Config;

use crate::TransactionMode;
use crate::database_error::DatabaseError;

use super::{creation::DatabaseCreation, database_client::DatabaseClient};

/// Configuration for the database connection pool.
#[derive(Debug, Clone, Default)]
pub struct PoolConfig {
    /// Maximum number of connections in the pool (default: 10)
    pub max_size: Option<usize>,
    /// Timeout waiting for a connection from the pool (default: 30s)
    pub wait_timeout_secs: Option<u64>,
    /// Timeout creating a new connection (default: 10s)
    pub create_timeout_secs: Option<u64>,
    /// Timeout recycling/validating a connection (default: 5s)
    pub recycle_timeout_secs: Option<u64>,
    /// Maximum lifetime of a connection in seconds before forced recycling (default: 900 = 15min)
    pub max_lifetime_secs: Option<u64>,
}

impl PoolConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_max_size(mut self, size: usize) -> Self {
        self.max_size = Some(size);
        self
    }

    pub fn with_wait_timeout(mut self, secs: u64) -> Self {
        self.wait_timeout_secs = Some(secs);
        self
    }

    pub fn with_create_timeout(mut self, secs: u64) -> Self {
        self.create_timeout_secs = Some(secs);
        self
    }

    pub fn with_recycle_timeout(mut self, secs: u64) -> Self {
        self.recycle_timeout_secs = Some(secs);
        self
    }

    pub fn with_max_lifetime(mut self, secs: u64) -> Self {
        self.max_lifetime_secs = Some(secs);
        self
    }
}

/// The current status of the connection pool.
#[derive(Debug, Clone)]
pub struct PoolStatus {
    /// Maximum number of connections in the pool
    pub max_size: usize,
    /// Current number of connections in the pool (both idle and in-use)
    pub size: usize,
    /// Number of idle connections available for immediate use
    pub available: usize,
    /// Number of tasks waiting for a connection
    pub waiting: usize,
}

pub struct DatabasePool {
    pool: Pool,
}

impl DatabasePool {
    /// Create a pool with legacy parameters (for backward compatibility)
    pub async fn create(
        creation: DatabaseCreation,
        pool_size: Option<usize>,
    ) -> Result<Self, DatabaseError> {
        let pool_config = PoolConfig {
            max_size: pool_size,
            ..Default::default()
        };
        Self::create_with_config(creation, pool_config).await
    }

    /// Create a pool with full configuration options
    pub async fn create_with_config(
        creation: DatabaseCreation,
        pool_config: PoolConfig,
    ) -> Result<Self, DatabaseError> {
        match creation {
            #[cfg(feature = "postgres-url")]
            DatabaseCreation::Url {
                url,
                transaction_mode,
            } => Self::from_db_url_with_config(&url, pool_config, transaction_mode).await,
            DatabaseCreation::Connect { config, connect } => {
                Self::from_connect_with_config(pool_config, *config, ConnectBridge(connect)).await
            }
        }
    }

    pub async fn get_client(&self) -> Result<DatabaseClient, DatabaseError> {
        match self.pool.get().await {
            Ok(client) => Ok(DatabaseClient::Pooled(client)),
            Err(err) => {
                let status = self.pool.status();
                tracing::error!(
                    error = %err,
                    pool_max_size = status.max_size,
                    pool_size = status.size,
                    pool_available = status.available,
                    pool_waiting = status.waiting,
                    "Failed to acquire database connection from pool"
                );
                Err(err.into())
            }
        }
    }

    /// Get the current status of the connection pool
    pub fn status(&self) -> PoolStatus {
        let status = self.pool.status();
        PoolStatus {
            max_size: status.max_size,
            size: status.size,
            available: status.available,
            waiting: status.waiting,
        }
    }

    #[cfg(feature = "postgres-url")]
    async fn from_db_url_with_config(
        url: &str,
        pool_config: PoolConfig,
        transaction_mode: TransactionMode,
    ) -> Result<Self, DatabaseError> {
        use std::str::FromStr;

        use crate::sql::connect::ssl_config::SslConfig;

        let (url, ssl_config) = SslConfig::from_url(url)?;

        let mut config = Config::from_str(&url).map_err(|e| {
            DatabaseError::Delegate(e)
                .with_context("Failed to parse PostgreSQL connection string".into())
        })?;

        transaction_mode.update_config(&mut config);

        match ssl_config {
            Some(ssl_config) => {
                let (config, tls) = ssl_config.updated_config(config)?;

                // If there is any TCP host, use the TLS connector (with the new Rustls version, SSL over unix sockets errors out)
                let has_tcp_hosts = config
                    .get_hosts()
                    .iter()
                    .any(|host| matches!(host, tokio_postgres::config::Host::Tcp(_)));

                if has_tcp_hosts {
                    Self::from_connect_with_config(pool_config, config, ConfigConnectImpl { tls })
                        .await
                } else {
                    Self::from_connect_with_config(
                        pool_config,
                        config,
                        ConfigConnectImpl {
                            tls: tokio_postgres::NoTls,
                        },
                    )
                    .await
                }
            }
            None => {
                Self::from_connect_with_config(
                    pool_config,
                    config,
                    ConfigConnectImpl {
                        tls: tokio_postgres::NoTls,
                    },
                )
                .await
            }
        }
    }

    /// Legacy method for backward compatibility
    pub async fn from_connect(
        pool_size: Option<usize>,
        config: Config,
        connect: impl Connect + 'static,
    ) -> Result<Self, DatabaseError> {
        let pool_config = PoolConfig {
            max_size: pool_size,
            ..Default::default()
        };
        Self::from_connect_with_config(pool_config, config, connect).await
    }

    pub async fn from_connect_with_config(
        pool_config: PoolConfig,
        config: Config,
        connect: impl Connect + 'static,
    ) -> Result<Self, DatabaseError> {
        // Validate connections when checked out so stale connections are not reused.
        let manager_config = ManagerConfig {
            recycling_method: RecyclingMethod::Verified,
        };

        let manager = Manager::from_connect(config, connect, manager_config);

        // Build pool with timeouts - requires runtime for timeout support
        let mut builder = Pool::builder(manager).runtime(Runtime::Tokio1);

        // Apply max_size
        if let Some(max_size) = pool_config.max_size {
            builder = builder.max_size(max_size);
        }

        // Apply timeouts (with sensible defaults for serverless DB compatibility)
        let wait_timeout = pool_config.wait_timeout_secs.unwrap_or(30);
        let create_timeout = pool_config.create_timeout_secs.unwrap_or(10);
        let recycle_timeout = pool_config.recycle_timeout_secs.unwrap_or(5);
        // Default max_lifetime to 15 minutes for Neon/serverless compatibility
        let max_lifetime_secs = pool_config.max_lifetime_secs.unwrap_or(900);

        builder = builder
            .wait_timeout(Some(Duration::from_secs(wait_timeout)))
            .create_timeout(Some(Duration::from_secs(create_timeout)))
            .recycle_timeout(Some(Duration::from_secs(recycle_timeout)));

        // Add pre_recycle hook to enforce max_lifetime
        // This rejects connections that are too old, causing the pool to create fresh ones
        let max_lifetime = Duration::from_secs(max_lifetime_secs);
        builder = builder.pre_recycle(Hook::sync_fn(move |_conn, metrics| {
            if metrics.age() > max_lifetime {
                tracing::debug!(
                    age_secs = metrics.age().as_secs(),
                    max_lifetime_secs = max_lifetime.as_secs(),
                    "Rejecting connection due to max_lifetime exceeded"
                );
                return Err(HookError::Message(
                    "Connection exceeded max_lifetime".into(),
                ));
            }
            Ok(())
        }));

        // Log pool configuration for debugging
        tracing::info!(
            max_size = pool_config.max_size.unwrap_or(10),
            wait_timeout_secs = wait_timeout,
            create_timeout_secs = create_timeout,
            recycle_timeout_secs = recycle_timeout,
            max_lifetime_secs = max_lifetime_secs,
            "Creating database connection pool"
        );

        let pool = builder.build().expect("Failed to create DB pool");

        let db = Self { pool };

        Ok(db)
    }
}

struct ConnectBridge(Box<dyn super::creation::Connect>);

impl Connect for ConnectBridge {
    fn connect(
        &self,
        pg_config: &tokio_postgres::Config,
    ) -> futures::future::BoxFuture<
        '_,
        Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>), tokio_postgres::Error>,
    > {
        self.0.connect(pg_config)
    }
}
