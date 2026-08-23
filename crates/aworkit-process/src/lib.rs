#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::needless_return,
    clippy::struct_excessive_bools
)]
//! Native lifecycle and operating-system primitives shared by Aworkit processes.
//!
//! This crate deliberately contains no product-domain policy. It supplies the
//! bounded process handshake plus fail-closed filesystem, process, IPC, and
//! identity adapters used by the trusted process boundaries.
//!
//! Capability reports intentionally expose independent Boolean guarantees, and
//! each fallible adapter's typed error is the authoritative exhaustive error set.
//! Registry poisoning is an internal invariant failure rather than a recoverable
//! operating-system boundary condition.

pub mod filesystem;
pub mod identity;
pub mod ipc;
pub mod runtime;
pub mod time;

pub use runtime::NativeProcessCapabilityReportV1;

use std::{env, io::Write, time::Duration};

use aworkit_protocol::{MAX_SAFE_WIRE_INTEGER, ProcessGeneration};

/// A process identity used exclusively by the startup smoke handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    /// Stable name written by the owning process binary.
    pub name: &'static str,
}

/// Starts one process in its generation and emits a bounded smoke handshake.
///
/// `--smoke` is intentionally synchronous: callers can verify startup without
/// a background service or product behavior. `--generation <n>` exercises the
/// generation fence now and will map to the protocol's generation type later.
pub fn launch(identity: ProcessIdentity) -> Result<(), String> {
    let mut args = env::args().skip(1);
    let mut smoke = false;
    let mut generation = ProcessGeneration(0);

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--smoke" => smoke = true,
            "--generation" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--generation requires an unsigned value".to_owned())?;
                generation = ProcessGeneration(
                    value
                        .parse()
                        .map_err(|_| "--generation must be an unsigned integer".to_owned())?,
                );
                if generation.0 > MAX_SAFE_WIRE_INTEGER {
                    return Err(format!(
                        "--generation must not exceed {MAX_SAFE_WIRE_INTEGER}"
                    ));
                }
            }
            "--help" | "-h" => {
                println!("Usage: {} [--smoke] [--generation <n>]", identity.name);
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    if !smoke {
        return Err(
            "this milestone scaffold only supports the bounded --smoke entry point".to_owned(),
        );
    }

    emit_handshake(identity, generation)
}

fn emit_handshake(identity: ProcessIdentity, generation: ProcessGeneration) -> Result<(), String> {
    let mut output = std::io::stdout().lock();
    writeln!(
        output,
        "aworkit-smoke process={} generation={} status=ready shutdown=bounded",
        identity.name, generation.0
    )
    .map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())?;

    // Keep shutdown behavior explicit and bounded without hosting a service yet.
    std::thread::sleep(Duration::from_millis(1));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_copyable_for_process_entry_points() {
        let identity = ProcessIdentity { name: "test" };
        assert_eq!(identity, identity);
    }
}
