//! Entrypoint for the isolated portable-session repository process seam.

fn main() {
    if let Err(error) = aworkit_process::launch(aworkit_process::ProcessIdentity {
        name: "portable-store",
    }) {
        eprintln!("portable-store: {error}");
        std::process::exit(2);
    }
}
