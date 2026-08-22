//! Entrypoint for the trusted, authority-owning application core.

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let result = if arguments.is_empty() || arguments == ["--serve"] {
        aworkit_trusted_core::serve_core_stdio(std::io::stdin().lock(), std::io::stdout().lock())
    } else {
        aworkit_process::launch(aworkit_process::ProcessIdentity {
            name: "trusted-core",
        })
    };
    if let Err(error) = result {
        eprintln!("trusted-core: {error}");
        std::process::exit(2);
    }
}
