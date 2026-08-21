//! Minimal lifecycle primitives shared by the isolated Aworkit processes.
//!
//! This crate deliberately contains no domain messages or privileged behavior.
//! It supplies only a bounded process handshake so the first milestone can prove
//! that process entry points start and terminate independently.

use std::{env, io::Write, time::Duration};

use aworkit_protocol::ProcessGeneration;

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
