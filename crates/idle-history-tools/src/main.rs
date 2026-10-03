//! Application-owned source imports over the `EditChain` engine.

mod cli;

fn main() -> std::process::ExitCode {
    cli::run()
}
