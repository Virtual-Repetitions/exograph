// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use crate::{Connect, database_error::DatabaseError};

use super::{creation::DatabaseCreation, database_client::DatabaseClient};

#[cfg(feature = "postgres-url")]
use super::creation::TransactionMode;

#[cfg(feature = "pool")]
use super::database_pool::{DatabasePool, PoolConfig, PoolStatus};

pub enum DatabaseClientManager {
    #[cfg(feature = "pool")]
    Pooled(DatabasePool),
    Direct(DatabaseCreation),
}

impl DatabaseClientManager {
    pub async fn from_connect_direct(
        check_connection: bool,
        config: tokio_postgres::Config,
        connect: impl Connect + 'static,
    ) -> Result<Self, DatabaseError> {
        let creation = DatabaseCreation::Connect {
            config: Box::new(config),
            connect: Box::new(connect),
        };

        let res = Ok(Self::Direct(creation));

        if let Ok(ref res) = res
            && check_connection
        {
            let _ = res.get_client().await?;
        }

        res
    }

    #[cfg(feature = "pool")]
    pub async fn from_connect_pooled(
        check_connection: bool,
        config: tokio_postgres::Config,
        connect: impl Connect + 'static,
        pool_size: usize,
    ) -> Result<Self, DatabaseError> {
        let creation = DatabaseCreation::Connect {
            config: Box::new(config),
            connect: Box::new(connect),
        };

        let res = Ok(Self::Pooled(
            DatabasePool::create(creation, Some(pool_size)).await?,
        ));

        if let Ok(ref res) = res
            && check_connection
        {
            let _ = res.get_client().await?;
        }

        res
    }

    pub async fn get_client(&self) -> Result<DatabaseClient, DatabaseError> {
        match self {
            #[cfg(feature = "pool")]
            DatabaseClientManager::Pooled(pool) => pool.get_client().await,
            DatabaseClientManager::Direct(creation) => creation.get_client().await,
        }
    }

    /// Get the current status of the connection pool (if using pooled connections)
    #[cfg(feature = "pool")]
    pub fn pool_status(&self) -> Option<PoolStatus> {
        match self {
            DatabaseClientManager::Pooled(pool) => Some(pool.status()),
            DatabaseClientManager::Direct(_) => None,
        }
    }
}

#[cfg(feature = "postgres-url")]
impl DatabaseClientManager {
    /// Create a database client manager from URL with legacy pool_size parameter
    pub async fn from_url(
        url: &str,
        check_connection: bool,
        #[allow(unused_variables)] pool_size: Option<usize>,
        transaction_mode: TransactionMode,
    ) -> Result<Self, DatabaseError> {
        #[cfg(feature = "pool")]
        {
            let pool_config = PoolConfig {
                max_size: pool_size,
                ..Default::default()
            };
            Self::from_url_with_pool_config(url, check_connection, pool_config, transaction_mode)
                .await
        }
        #[cfg(not(feature = "pool"))]
        {
            Self::from_url_direct(url, check_connection, transaction_mode).await
        }
    }

    /// Create a database client manager from URL with full pool configuration
    #[cfg(feature = "pool")]
    pub async fn from_url_with_pool_config(
        url: &str,
        check_connection: bool,
        pool_config: PoolConfig,
        transaction_mode: TransactionMode,
    ) -> Result<Self, DatabaseError> {
        let creation = DatabaseCreation::Url {
            url: url.to_string(),
            transaction_mode,
        };
        let res = Ok(Self::Pooled(
            DatabasePool::create_with_config(creation, pool_config).await?,
        ));

        if let Ok(ref res) = res
            && check_connection
        {
            let _ = res.get_client().await?;
        }

        res
    }

    pub async fn from_url_direct(
        url: &str,
        check_connection: bool,
        transaction_mode: TransactionMode,
    ) -> Result<Self, DatabaseError> {
        let creation = DatabaseCreation::Url {
            url: url.to_string(),
            transaction_mode,
        };
        let res = Ok(DatabaseClientManager::Direct(creation));

        if let Ok(ref res) = res
            && check_connection
        {
            let _ = res.get_client().await?;
        }

        res
    }

    /// Legacy method - use from_url_with_pool_config for full configuration
    #[cfg(feature = "pool")]
    pub async fn from_url_pooled(
        url: &str,
        check_connection: bool,
        pool_size: Option<usize>,
        transaction_mode: TransactionMode,
    ) -> Result<Self, DatabaseError> {
        let pool_config = PoolConfig {
            max_size: pool_size,
            ..Default::default()
        };
        Self::from_url_with_pool_config(url, check_connection, pool_config, transaction_mode).await
    }
}
