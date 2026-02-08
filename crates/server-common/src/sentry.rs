use exo_env::Environment;
use std::sync::OnceLock;

static SENTRY_GUARD: OnceLock<sentry::ClientInitGuard> = OnceLock::new();

fn get_env_value(env: &dyn Environment, key: &str) -> Option<String> {
    env.get(key)
        .map(|value| value.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn init(env: &dyn Environment) {
    if SENTRY_GUARD.get().is_some() {
        return;
    }

    let dsn = get_env_value(env, "EXO_SENTRY_DSN").or_else(|| get_env_value(env, "SENTRY_DSN"));

    let Some(dsn) = dsn else {
        return;
    };

    let environment = get_env_value(env, "EXO_SENTRY_ENVIRONMENT")
        .or_else(|| get_env_value(env, "SENTRY_ENVIRONMENT"))
        .or_else(|| get_env_value(env, "EXO_ENV"));

    let release =
        get_env_value(env, "EXO_SENTRY_RELEASE").or_else(|| get_env_value(env, "SENTRY_RELEASE"));

    let sample_rate = get_env_value(env, "EXO_SENTRY_SAMPLE_RATE")
        .or_else(|| get_env_value(env, "SENTRY_SAMPLE_RATE"))
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(1.0);

    let traces_sample_rate = get_env_value(env, "EXO_SENTRY_TRACES_SAMPLE_RATE")
        .or_else(|| get_env_value(env, "SENTRY_TRACES_SAMPLE_RATE"))
        .and_then(|value| value.parse::<f32>().ok())
        .unwrap_or(0.0);

    let options = sentry::ClientOptions {
        dsn: dsn.parse().ok(),
        environment: environment.map(Into::into),
        release: release.map(Into::into),
        sample_rate,
        traces_sample_rate,
        ..Default::default()
    };

    let guard = sentry::init(options);
    let _ = SENTRY_GUARD.set(guard);
}

#[allow(dead_code)]
pub fn enabled() -> bool {
    sentry::Hub::current().client().is_some()
}
