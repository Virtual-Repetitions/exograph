use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};

use common::download::{download_dir_if_needed, exo_cache_root};
use core_model_builder::error::ModelBuildingError;
use deno_core::ModuleType;
use exo_deno::deno_executor_pool::{DenoScriptDefn, ResolvedModule};
use tokio::{process::Command, runtime::Handle, task::block_in_place};
use url::Url;

const DENO_VERSION: &str = "2.5.4";
const DENO_BUNDLE_WARNING: &[u8] = b"is experimental and subject to changes";

pub fn bundle_computed_script(
    module_fs_path: &Path,
) -> Result<(String, Vec<u8>), ModelBuildingError> {
    let module_fs_path = module_fs_path.to_path_buf();

    match Handle::try_current() {
        Ok(handle) => {
            let handle = handle.clone();
            block_in_place(move || handle.block_on(bundle_and_serialize(module_fs_path)))
        }
        Err(_) => {
            let runtime = tokio::runtime::Runtime::new().map_err(|e| {
                ModelBuildingError::Generic(format!("Failed to initialize Tokio runtime: {e}"))
            })?;

            runtime.block_on(bundle_and_serialize(module_fs_path))
        }
    }
}

async fn bundle_and_serialize(
    module_fs_path: PathBuf,
) -> Result<(String, Vec<u8>), ModelBuildingError> {
    let bundled = bundle_source(&module_fs_path).await?;

    let canonical = std::fs::canonicalize(&module_fs_path).map_err(|e| {
        ModelBuildingError::Generic(format!(
            "Failed to canonicalize computed script path {}: {e}",
            module_fs_path.to_string_lossy()
        ))
    })?;

    let root = Url::from_file_path(&canonical).map_err(|_| {
        ModelBuildingError::Generic(format!(
            "Failed to construct URL for computed script {}",
            canonical.to_string_lossy()
        ))
    })?;

    let script_defn = DenoScriptDefn {
        modules: HashMap::from([(
            root.clone(),
            ResolvedModule::Module(bundled, ModuleType::JavaScript, root.clone(), false),
        )]),
    };

    let serialized =
        serde_json::to_vec(&script_defn).map_err(|e| ModelBuildingError::Generic(e.to_string()))?;

    Ok((root.to_string(), serialized))
}

async fn bundle_source(module_fs_path: &Path) -> Result<String, ModelBuildingError> {
    let deno_path = download_deno_if_needed().await?;

    let output = Command::new(deno_path)
        .arg("bundle")
        .arg("--allow-import")
        .arg("--quiet")
        .arg("--node-modules-dir=auto")
        .arg(module_fs_path.to_string_lossy().as_ref())
        .output()
        .await;

    fn simplify_error(output: &[u8]) -> String {
        let output = output
            .split(|b| *b == b'\n')
            .filter(|line| !line.ends_with(DENO_BUNDLE_WARNING))
            .collect::<Vec<_>>()
            .join(&b'\n');

        let output_str = String::from_utf8_lossy(&output).to_string();

        let current_dir_url = Url::from_directory_path(
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
        )
        .unwrap()
        .to_string();

        output_str.replace(&current_dir_url, "")
    }

    match output {
        Ok(output) => {
            if !output.status.success() {
                Err(ModelBuildingError::TSJSParsingError(simplify_error(
                    &output.stderr,
                )))
            } else {
                String::from_utf8(output.stdout).map_err(|e| {
                    ModelBuildingError::Generic(format!(
                        "Failed to parse bundled output as UTF-8: {e}"
                    ))
                })
            }
        }
        Err(e) => Err(ModelBuildingError::Generic(format!(
            "Failed to execute Deno: {e}"
        ))),
    }
}

async fn download_deno_if_needed() -> Result<PathBuf, ModelBuildingError> {
    let deno_executable = if env::consts::OS == "windows" {
        "deno.exe"
    } else {
        "deno"
    };
    let _deno_path = exo_cache_root()
        .map_err(|e| {
            ModelBuildingError::Generic(format!("Failed to determine cache root directory: {e}"))
        })?
        .join("deno")
        .join(DENO_VERSION)
        .join(deno_executable);

    let target_os = env::consts::OS;
    let target_arch = env::consts::ARCH;

    let platform = match (target_os, target_arch) {
        ("macos", "x86_64") => {
            return Err(ModelBuildingError::Generic(
                "Intel Macs (x86_64) are no longer supported.".to_string(),
            ));
        }
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => {
            return Err(ModelBuildingError::Generic(format!(
                "Unsupported platform: {os}-{arch}"
            )));
        }
    };

    download_dir_if_needed(
        &format!(
            "https://github.com/denoland/deno/releases/download/v{DENO_VERSION}/deno-{platform}.zip"
        ),
        "Deno",
        &format!("deno/{DENO_VERSION}"),
    )
    .await
    .map(|path| path.join(deno_executable))
    .map_err(|e| ModelBuildingError::Generic(format!("Failed to download Deno: {e}")))
}
