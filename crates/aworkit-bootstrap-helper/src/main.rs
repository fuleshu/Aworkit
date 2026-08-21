//! Entrypoint for the independently surviving bootstrap and rollback helper.

fn main() {
    if let Err(error) = aworkit_process::launch(aworkit_process::ProcessIdentity {
        name: "bootstrap-helper",
    }) {
        eprintln!("bootstrap-helper: {error}");
        std::process::exit(2);
    }
}
