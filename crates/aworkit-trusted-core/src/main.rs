//! Entrypoint for the trusted, authority-owning application core.

fn main() {
    if let Err(error) = aworkit_process::launch(aworkit_process::ProcessIdentity {
        name: "trusted-core",
    }) {
        eprintln!("trusted-core: {error}");
        std::process::exit(2);
    }
}
