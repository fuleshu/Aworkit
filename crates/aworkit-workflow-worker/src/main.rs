//! Entrypoint for the unprivileged workflow runtime worker.

fn main() {
    if let Err(error) = aworkit_process::launch(aworkit_process::ProcessIdentity {
        name: "workflow-worker",
    }) {
        eprintln!("workflow-worker: {error}");
        std::process::exit(2);
    }
}
