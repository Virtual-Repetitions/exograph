// Copyright Exograph, Inc. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file at the root of this repository.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::Arc,
    time::{Duration, Instant},
};

use deno_core::{Extension, ModuleType, url::Url};
use exo_env::Environment;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::{Arg, error::DenoError};

use super::{
    deno_actor::DenoActor,
    deno_executor::{CallbackProcessor, DenoExecutor},
    deno_module::{DenoModule, UserCode},
};

use std::fmt::Debug;

type DenoActorPoolMap<C, M, R> = HashMap<String, DenoActorPool<C, M, R>>;
type DenoActorPool<C, M, R> = Vec<PooledActor<C, M, R>>;

/// Maximum number of `DenoActor`s (V8 isolates) kept per module. Each actor
/// holds the module's full bundle in a V8 heap, so an unbounded pool converts
/// the peak concurrency of each module into a permanent memory floor.
const EXO_DENO_POOL_MAX_ACTORS_PER_MODULE: &str = "EXO_DENO_POOL_MAX_ACTORS_PER_MODULE";

/// Default cap scales with compute: actors execute JS on a CPU, so there is no
/// throughput to gain from many more runnable isolates per module than vCPUs —
/// extra ones only occupy memory. `available_parallelism` respects cgroup
/// limits, so container/VM resizes adjust this automatically.
fn default_max_actors_per_module() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_mul(2)
        .max(2)
}

/// Reap actors that have been idle for this long (seconds). 0 disables reaping.
const EXO_DENO_POOL_IDLE_TIMEOUT_SECS: &str = "EXO_DENO_POOL_IDLE_TIMEOUT_SECS";
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;

struct PooledActor<C, M, R> {
    actor: DenoActor<C, M, R>,
    last_used: Instant,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ResolvedModule {
    Module(String, ModuleType, Url, bool),
    Redirect(Url),
}

/// Serialized type for modules loaded during the build phase
#[derive(Serialize, Deserialize, Debug)]
pub struct DenoScriptDefn {
    pub modules: HashMap<Url, ResolvedModule>,
}

pub struct DenoExecutorConfig<C> {
    shims: Vec<(&'static str, &'static [&'static str])>,
    additional_code: Vec<&'static str>,
    explicit_error_class_name: Option<&'static str>,
    create_extensions: fn() -> Vec<Extension>,
    process_call_context: fn(&mut DenoModule, C) -> (),
    additional_env: Arc<dyn Environment>,
}

impl<C> DenoExecutorConfig<C> {
    pub fn new(
        shims: Vec<(&'static str, &'static [&'static str])>,
        additional_code: Vec<&'static str>,
        explicit_error_class_name: Option<&'static str>,
        create_extensions: fn() -> Vec<Extension>,
        process_call_context: fn(&mut DenoModule, C) -> (),
        additional_env: Arc<dyn Environment>,
    ) -> Self {
        Self {
            shims,
            additional_code,
            explicit_error_class_name,
            create_extensions,
            process_call_context,
            additional_env,
        }
    }
}

/// DenoExecutorPool maintains a pool of `DenoActor`s for each module to delegate work to.
///
/// Calling `execute` will either select a free actor or allocate a new `DenoActor` to run the function on.
/// It will create a `DenoExecutor` with that actor and delegate the method execution to it.
///
/// The hierarchy of modules:
///
/// DenoExecutorPool -> DenoExecutor -> DenoActor -> DenoModule
///                  -> DenoExecutor -> DenoActor -> DenoModule
///                  -> DenoExecutor -> DenoActor -> DenoModule
///
/// # Type Parameters
/// - `C`: The type of the call context (for example, `Option<InterceptedOperationName>`). This object
///   is set into the `DenoModule`s GothamState and may be resolved synchronously or asynchronously.
/// - `M`: The type of the callback message.
/// - `R`: An opaque return type to also return from GothamStorage with each method execution. Useful for
///   returning out-of-band information that should not be a part of the return value.
pub struct DenoExecutorPool<C, M, R> {
    config: DenoExecutorConfig<C>,
    actor_pool_map: Arc<Mutex<DenoActorPoolMap<C, M, R>>>,
    max_actors_per_module: usize,
    return_type: PhantomData<R>,
}

fn env_usize(env: &dyn Environment, key: &str, default: usize) -> usize {
    env.get(key)
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

impl<C: Sync + Send + Debug + 'static, M: Sync + Send + 'static, R: Sync + Send + Debug + 'static>
    DenoExecutorPool<C, M, R>
{
    pub fn new(
        shims: Vec<(&'static str, &'static [&'static str])>,
        additional_code: Vec<&'static str>,
        explicit_error_class_name: Option<&'static str>,
        create_extensions: fn() -> Vec<Extension>,
        process_call_context: fn(&mut DenoModule, C) -> (),
        additional_env: Arc<dyn Environment>,
    ) -> Self {
        Self::new_from_config(DenoExecutorConfig::new(
            shims,
            additional_code,
            explicit_error_class_name,
            create_extensions,
            process_call_context,
            additional_env,
        ))
    }

    pub fn new_from_config(config: DenoExecutorConfig<C>) -> Self {
        let env = config.additional_env.as_ref();
        let max_actors_per_module = env_usize(
            env,
            EXO_DENO_POOL_MAX_ACTORS_PER_MODULE,
            default_max_actors_per_module(),
        )
        .max(1);
        let idle_timeout_secs = env_usize(
            env,
            EXO_DENO_POOL_IDLE_TIMEOUT_SECS,
            DEFAULT_IDLE_TIMEOUT_SECS as usize,
        ) as u64;

        let actor_pool_map: Arc<Mutex<DenoActorPoolMap<C, M, R>>> =
            Arc::new(Mutex::new(DenoActorPoolMap::default()));

        // Reap idle actors so a burst of traffic doesn't become a permanent
        // memory floor. Weak reference lets the task end when the pool drops.
        // Skipped when no tokio runtime is available (e.g. sync construction in tests).
        if idle_timeout_secs > 0
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            let map_weak = Arc::downgrade(&actor_pool_map);
            let idle = Duration::from_secs(idle_timeout_secs);
            handle.spawn(async move {
                let mut interval = tokio::time::interval(idle / 4);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    interval.tick().await;
                    let Some(map) = map_weak.upgrade() else { break };
                    let mut map = map.lock().await;
                    for (path, pool) in map.iter_mut() {
                        let before = pool.len();
                        pool.retain(|p| p.actor.is_busy() || p.last_used.elapsed() < idle);
                        let reaped = before - pool.len();
                        if reaped > 0 {
                            tracing::debug!(module = %path, reaped, remaining = pool.len(),
                                "Reaped idle Deno actors");
                        }
                    }
                    map.retain(|_, pool| !pool.is_empty());
                }
            });
        }

        Self {
            config,
            actor_pool_map,
            max_actors_per_module,
            return_type: PhantomData,
        }
    }

    // Execute a method and obtain its result
    pub async fn execute(
        &self,
        script_path: &str,
        script: DenoScriptDefn,
        method_name: &str,
        arguments: Vec<Arg>,
        call_context: C,
        callback_processor: impl CallbackProcessor<M>,
    ) -> Result<Value, DenoError> {
        let (result, _) = self
            .execute_and_get_r(
                script_path,
                script,
                method_name,
                arguments,
                call_context,
                callback_processor,
            )
            .await?;
        Ok(result)
    }

    // execute(...), but also return R from Deno's GothamStorage
    pub async fn execute_and_get_r(
        &self,
        script_path: &str,
        script: DenoScriptDefn,
        method_name: &str,
        arguments: Vec<Arg>,
        call_context: C,
        callback_processor: impl CallbackProcessor<M>,
    ) -> Result<(Value, Option<R>), DenoError> {
        let executor = self.get_executor(script_path, script).await?;
        executor
            .execute(method_name, arguments, call_context, callback_processor)
            .await
    }

    // TODO: look at passing a fn pointer struct as an argument
    async fn get_executor(
        &self,
        script_path: &str,
        script: DenoScriptDefn,
    ) -> Result<DenoExecutor<C, M, R>, DenoError> {
        // Find a free pooled actor, or grow the pool up to the per-module cap.
        // At the cap with all actors busy, create a TRANSIENT actor that is not
        // pooled and is dropped after this execution. Never wait: executions can
        // nest (a Deno resolver calling back into GraphQL may need another actor
        // of the same module), so blocking here can deadlock — stuck requests
        // then hold DB connections until the whole pool starves.
        {
            let mut actor_pool_map = self.actor_pool_map.lock().await;
            let actor_pool = actor_pool_map.entry(script_path.to_string()).or_default();

            if let Some(pooled) = actor_pool.iter_mut().find(|p| !p.actor.is_busy()) {
                pooled.last_used = Instant::now();
                return Ok(DenoExecutor {
                    actor: pooled.actor.clone(),
                });
            }

            if actor_pool.len() < self.max_actors_per_module {
                let new_actor = self.create_actor(script_path, script)?;
                actor_pool.push(PooledActor {
                    actor: new_actor.clone(),
                    last_used: Instant::now(),
                });
                return Ok(DenoExecutor { actor: new_actor });
            }
        }

        // Overflow: pay isolate startup for this one execution rather than
        // permanently growing the pool (memory floor) or waiting (deadlock).
        tracing::debug!(
            module = %script_path,
            max_actors = self.max_actors_per_module,
            "Deno actor pool at cap; creating transient overflow actor"
        );
        let transient_actor = self.create_actor(script_path, script)?;
        Ok(DenoExecutor {
            actor: transient_actor,
        })
    }

    fn create_actor(
        &self,
        script_path: &str,
        script: DenoScriptDefn,
    ) -> Result<DenoActor<C, M, R>, DenoError> {
        DenoActor::new(
            UserCode::LoadFromMemory {
                path: script_path.to_owned(),
                script,
            },
            self.config.shims.clone(),
            self.config.additional_code.clone(),
            self.config.create_extensions,
            self.config.explicit_error_class_name,
            self.config.process_call_context,
            self.config.additional_env.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deno_module::Arg;
    use deno_core::ModuleSpecifier;
    use exo_env::MapEnvironment;
    use serde_json::Value;

    use futures::future::join_all;

    #[tokio::test]
    async fn test_actor_executor() {
        let module_path = "file://test_js/direct.js";
        let module_script = include_str!("test_js/direct.js").to_string();

        let executor_pool = DenoExecutorPool::<(), (), ()>::new(
            vec![],
            vec![],
            None,
            Vec::new,
            |_, _| {},
            Arc::new(MapEnvironment::default()),
        );

        let res = executor_pool
            .execute(
                module_path,
                DenoScriptDefn {
                    modules: vec![(
                        ModuleSpecifier::parse(module_path).unwrap(),
                        ResolvedModule::Module(
                            module_script,
                            ModuleType::JavaScript,
                            ModuleSpecifier::parse(module_path).unwrap(),
                            false,
                        ),
                    )]
                    .into_iter()
                    .collect(),
                },
                "addAndDouble",
                vec![Arg::Serde(2.into()), Arg::Serde(3.into())],
                (),
                (),
            )
            .await;

        assert_eq!(res.unwrap(), 10);
    }

    #[tokio::test]
    async fn test_actor_executor_concurrent() {
        let module_path = "file://test_js/direct.js";
        let module_script = include_str!("test_js/direct.js").to_string();

        let executor_pool = DenoExecutorPool::new(
            vec![],
            vec![],
            None,
            Vec::new,
            |_, _| {},
            Arc::new(MapEnvironment::default()),
        );

        let total_futures = 10;

        let mut handles = vec![];

        async fn execute_function(
            pool: &DenoExecutorPool<(), (), ()>,
            script_path: &str,
            script: String,
            method_name: &str,
            arguments: Vec<Arg>,
        ) -> Result<Value, DenoError> {
            pool.execute(
                script_path,
                DenoScriptDefn {
                    modules: vec![(
                        ModuleSpecifier::parse(script_path).unwrap(),
                        ResolvedModule::Module(
                            script,
                            ModuleType::JavaScript,
                            ModuleSpecifier::parse(script_path).unwrap(),
                            false,
                        ),
                    )]
                    .into_iter()
                    .collect(),
                    // npm_snapshot: None,
                },
                method_name,
                arguments,
                (),
                (),
            )
            .await
        }

        for _ in 1..=total_futures {
            let handle = execute_function(
                &executor_pool,
                module_path,
                module_script.clone(),
                "addAndDouble",
                vec![
                    Arg::Serde(Value::Number(4.into())),
                    Arg::Serde(Value::Number(2.into())),
                ],
            );

            handles.push(handle);
        }

        let result = join_all(handles)
            .await
            .iter()
            .filter(|res| res.as_ref().unwrap() == 12)
            .count();

        assert_eq!(result, total_futures);

        // 10 concurrent executions must not grow the pool past the cap
        let cap = default_max_actors_per_module();
        let pool_size = executor_pool
            .actor_pool_map
            .lock()
            .await
            .get(module_path)
            .map(|pool| pool.len())
            .unwrap_or(0);
        assert!(
            pool_size >= 1 && pool_size <= cap,
            "pool size {pool_size} exceeds cap {cap}"
        );
    }

    #[tokio::test]
    async fn test_actor_executor_concurrent_at_cap_one() {
        // With the pool capped at a single actor, concurrent executions must
        // still all complete (overflow actors, never waiting — waiting can
        // deadlock when executions nest) and the pool must stay at the cap.
        let module_path = "file://test_js/direct.js";
        let module_script = include_str!("test_js/direct.js").to_string();

        let mut env = MapEnvironment::new();
        env.set("EXO_DENO_POOL_MAX_ACTORS_PER_MODULE", "1");

        let executor_pool = DenoExecutorPool::<(), (), ()>::new(
            vec![],
            vec![],
            None,
            Vec::new,
            |_, _| {},
            Arc::new(env),
        );

        let handles = (0..10).map(|_| {
            executor_pool.execute(
                module_path,
                DenoScriptDefn {
                    modules: vec![(
                        ModuleSpecifier::parse(module_path).unwrap(),
                        ResolvedModule::Module(
                            module_script.clone(),
                            ModuleType::JavaScript,
                            ModuleSpecifier::parse(module_path).unwrap(),
                            false,
                        ),
                    )]
                    .into_iter()
                    .collect(),
                },
                "addAndDouble",
                vec![
                    Arg::Serde(Value::Number(4.into())),
                    Arg::Serde(Value::Number(2.into())),
                ],
                (),
                (),
            )
        });

        let ok = join_all(handles)
            .await
            .iter()
            .filter(|res| res.as_ref().unwrap() == 12)
            .count();
        assert_eq!(ok, 10);

        let pool_size = executor_pool
            .actor_pool_map
            .lock()
            .await
            .get(module_path)
            .map(|pool| pool.len())
            .unwrap_or(0);
        assert_eq!(pool_size, 1, "pool must stay at cap 1, got {pool_size}");
    }
}
