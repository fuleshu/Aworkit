//! Entrypoint for the unprivileged workflow runtime worker.

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let result = if arguments.is_empty() || arguments == ["--serve"] {
        aworkit_workflow_worker::serve_stdio(std::io::stdin().lock(), std::io::stdout().lock())
            .map_err(|error| error.to_string())
    } else {
        aworkit_process::launch(aworkit_process::ProcessIdentity {
            name: "workflow-worker",
        })
    };
    if let Err(error) = result {
        eprintln!("workflow-worker: {error}");
        std::process::exit(2);
    }
}
