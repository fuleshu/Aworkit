//! Entrypoint for the core-supervised capability and extension host.

fn main() {
    if let Err(error) = aworkit_process::launch(aworkit_process::ProcessIdentity {
        name: "capability-host",
    }) {
        eprintln!("capability-host: {error}");
        std::process::exit(2);
    }
}
