//! Command-line entrypoint for deterministic release-bundle assembly.

use std::{collections::BTreeMap, path::PathBuf};

use aworkit_bootstrap_helper::release::{WholeBundleInputsV1, assemble_whole_application_bundle};

fn main() {
    if let Err(error) = run() {
        eprintln!("aworkit-release-assembler: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut values = BTreeMap::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(flag) = arguments.next() {
        if !flag.starts_with("--") {
            return Err(format!("unexpected argument {flag}"));
        }
        let value = arguments
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        if values.insert(flag.clone(), value).is_some() {
            return Err(format!("duplicate argument {flag}"));
        }
    }
    let required = |name: &str| {
        values
            .get(name)
            .cloned()
            .ok_or_else(|| format!("missing required argument {name}"))
    };
    let inputs = WholeBundleInputsV1 {
        output: PathBuf::from(required("--output")?),
        desktop: PathBuf::from(required("--desktop")?),
        trusted_core: PathBuf::from(required("--trusted-core")?),
        workflow_worker: PathBuf::from(required("--workflow-worker")?),
        capability_host: PathBuf::from(required("--capability-host")?),
        bootstrap_helper: PathBuf::from(required("--bootstrap-helper")?),
        ui_dist: PathBuf::from(required("--ui-dist")?),
        source_revision: required("--source-revision")?,
        source_tree_hash: required("--source-tree-hash")?,
        workspace_identity_hash: required("--workspace-identity-hash")?,
        toolchain_hash: required("--toolchain-hash")?,
        source_date_epoch: required("--source-date-epoch")?
            .parse()
            .map_err(|_| "--source-date-epoch must be a nonnegative integer".to_owned())?,
    };
    let manifest = assemble_whole_application_bundle(&inputs).map_err(|error| error.to_string())?;
    println!("{}", manifest.provenance.provenance_hash);
    Ok(())
}
