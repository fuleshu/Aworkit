//! Entrypoint for the isolated local-state and evidence-store process seam.

fn main() {
    if let Err(error) = aworkit_process::launch(aworkit_process::ProcessIdentity {
        name: "local-store",
    }) {
        eprintln!("local-store: {error}");
        std::process::exit(2);
    }
}
