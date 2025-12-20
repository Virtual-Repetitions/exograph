// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::{io::BufRead, path::Path, process::Command};

use super::{
    docker::DockerPostgresDatabaseServer, error::EphemeralDatabaseSetupError,
    local::LocalPostgresDatabaseServer,
};

enum EphemeralDatabaseLaunchPreference {
    PreferLocal,
    PreferDocker,
    LocalOnly,
    DockerOnly,
}

pub const EXO_SQL_EPHEMERAL_DATABASE_LAUNCH_PREFERENCE: &str =
    "EXO_SQL_EPHEMERAL_DATABASE_LAUNCH_PREFERENCE";

/// Launcher for an ephemeral database server using either a local Postgres installation or Docker
pub struct EphemeralDatabaseLauncher {
    preference: EphemeralDatabaseLaunchPreference,
}

impl EphemeralDatabaseLauncher {
    pub fn from_env() -> Self {
        let preference_env = std::env::var(EXO_SQL_EPHEMERAL_DATABASE_LAUNCH_PREFERENCE);

        let preference = match preference_env.as_deref().unwrap_or("prefer-local") {
            "prefer-local" => EphemeralDatabaseLaunchPreference::PreferLocal,
            "prefer-docker" => EphemeralDatabaseLaunchPreference::PreferDocker,
            "local-only" => EphemeralDatabaseLaunchPreference::LocalOnly,
            "docker-only" => EphemeralDatabaseLaunchPreference::DockerOnly,
            _ => {
                tracing::error!(
                    "Invalid value for EXO_SQL_EPHEMERAL_DATABASE_LAUNCH_PREFERENCE: {preference_env:?}"
                );
                EphemeralDatabaseLaunchPreference::PreferLocal
            }
        };

        Self { preference }
    }

    fn create_local_server()
    -> Result<Box<dyn EphemeralDatabaseServer + Send + Sync>, EphemeralDatabaseSetupError> {
        let local_available = LocalPostgresDatabaseServer::check_availability();
        if let Ok(true) = local_available {
            tracing::info!("Launching PostgreSQL locally...");
            LocalPostgresDatabaseServer::start()
        } else {
            tracing::error!("Local PostgreSQL is not available");
            Err(EphemeralDatabaseSetupError::Generic(
                "Local PostgreSQL is not available".to_string(),
            ))
        }
    }

    fn create_docker_server()
    -> Result<Box<dyn EphemeralDatabaseServer + Send + Sync>, EphemeralDatabaseSetupError> {
        let docker_available = DockerPostgresDatabaseServer::check_availability();
        if let Ok(true) = docker_available {
            tracing::info!("Launching PostgreSQL in Docker...");
            DockerPostgresDatabaseServer::start()
        } else {
            tracing::error!("Docker PostgreSQL is not available");
            Err(EphemeralDatabaseSetupError::Generic(
                "Docker PostgreSQL is not available".to_string(),
            ))
        }
    }

    /// Create a new database server.
    /// Currently, it prefers a local installation, but falls back to Docker if it's not available
    pub fn create_server(
        &self,
    ) -> Result<Box<dyn EphemeralDatabaseServer + Send + Sync>, EphemeralDatabaseSetupError> {
        match self.preference {
            EphemeralDatabaseLaunchPreference::PreferLocal => {
                let local_error = match Self::local_pgvector_available() {
                    Ok(true) => match Self::create_local_server() {
                        Ok(server) => return Ok(server),
                        Err(e) => {
                            tracing::warn!("Failed to launch local PostgreSQL instance: {}", e);
                            Some(format!("Failed to launch local PostgreSQL instance: {e}"))
                        }
                    },
                    Ok(false) => {
                        tracing::warn!(
                            "Local PostgreSQL installation does not include the pgvector extension; falling back to Docker"
                        );
                        Some(
                            "Local PostgreSQL installation does not include the pgvector extension"
                                .to_string(),
                        )
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to verify pgvector support for local PostgreSQL installation: {}",
                            e
                        );
                        Some(format!("Failed to verify pgvector support: {e}"))
                    }
                };

                match Self::create_docker_server() {
                    Ok(server) => Ok(server),
                    Err(docker_error) => {
                        if let Some(local_error) = local_error {
                            Err(EphemeralDatabaseSetupError::Generic(format!(
                                "{local_error}. Additionally, failed to launch Docker PostgreSQL: {docker_error}"
                            )))
                        } else {
                            Err(docker_error)
                        }
                    }
                }
            }
            EphemeralDatabaseLaunchPreference::PreferDocker => match Self::create_docker_server() {
                Ok(server) => Ok(server),
                Err(docker_error) => {
                    tracing::warn!(
                        "Failed to launch Docker PostgreSQL instance: {}. Falling back to local installation.",
                        docker_error
                    );
                    match Self::local_pgvector_available() {
                        Ok(true) => Self::create_local_server(),
                        Ok(false) => Err(EphemeralDatabaseSetupError::Generic(
                            "Local PostgreSQL installation does not include the pgvector extension. Install the extension or set EXO_SQL_EPHEMERAL_DATABASE_LAUNCH_PREFERENCE=docker-only."
                                .to_string(),
                        )),
                        Err(e) => Err(EphemeralDatabaseSetupError::Generic(format!(
                            "Failed to verify pgvector support for local PostgreSQL installation: {e}"
                        ))),
                    }
                }
            },
            EphemeralDatabaseLaunchPreference::LocalOnly => {
                match Self::local_pgvector_available() {
                    Ok(true) => Self::create_local_server(),
                    Ok(false) => Err(EphemeralDatabaseSetupError::Generic(
                        "Local PostgreSQL installation does not include the pgvector extension. Install the extension or set EXO_SQL_EPHEMERAL_DATABASE_LAUNCH_PREFERENCE=docker-only."
                            .to_string(),
                    )),
                    Err(e) => Err(EphemeralDatabaseSetupError::Generic(format!(
                        "Failed to verify pgvector support for local PostgreSQL installation: {e}"
                    ))),
                }
            }
            EphemeralDatabaseLaunchPreference::DockerOnly => Self::create_docker_server(),
        }
    }

    fn local_pgvector_available() -> Result<bool, EphemeralDatabaseSetupError> {
        let output = Command::new("pg_config")
            .arg("--sharedir")
            .output()
            .map_err(|e| {
                EphemeralDatabaseSetupError::Generic(format!("Failed to execute pg_config: {e}"))
            })?;

        if !output.status.success() {
            return Err(EphemeralDatabaseSetupError::Generic(format!(
                "pg_config --sharedir exited with status {status}",
                status = output.status
            )));
        }

        let sharedir = String::from_utf8(output.stdout).map_err(|e| {
            EphemeralDatabaseSetupError::Generic(format!("Failed to parse pg_config output: {e}"))
        })?;

        let sharedir = sharedir.trim();
        let control_file = Path::new(sharedir).join("extension").join("vector.control");

        Ok(control_file.exists())
    }
}

/// A ephemeral database server that can create ephemeral databases
/// Implemented should implement `Drop` to clean up the server to free up resources
pub trait EphemeralDatabaseServer {
    /// Create a new database on the server with the specified name
    fn create_database(
        &self,
        name: &str,
    ) -> Result<Box<dyn EphemeralDatabase + Send + Sync>, EphemeralDatabaseSetupError>;

    fn cleanup(&self);
}

/// A ephemeral database that can be used for testing.
/// Implemented should implement `Drop` to clean up the database to free up resources
pub trait EphemeralDatabase {
    /// Get the URL to connect to the database. The URL should be in the format suitable as the `psql` argument
    fn url(&self) -> String;
}

/// A utility function to launch a process and wait for it to exit
pub(super) fn launch_process(
    name: &str,
    args: &[&str],
    report_errors: bool,
) -> Result<(), EphemeralDatabaseSetupError> {
    let mut command = std::process::Command::new(name)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            EphemeralDatabaseSetupError::Generic(format!("Failed to launch process {name}: {e}"))
        })?;

    let status = command.wait().map_err(|e| {
        EphemeralDatabaseSetupError::Generic(format!("Failed to wait for process {name}: {e}"))
    })?;

    if !status.success() {
        if report_errors && let Some(stderr) = command.stderr.take() {
            let stderr = std::io::BufReader::new(stderr);
            stderr.lines().for_each(|line| {
                tracing::error!("{}: {}", name, line.unwrap());
            });
        }
        return Err(EphemeralDatabaseSetupError::Generic(format!(
            "Process {name} exited with non-zero status code {status}",
        )));
    }
    Ok(())
}

pub(crate) fn generate_random_string() -> String {
    use rand::Rng;

    rand::rng()
        .sample_iter(&rand::distr::Alphanumeric)
        .take(15)
        .map(char::from)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
