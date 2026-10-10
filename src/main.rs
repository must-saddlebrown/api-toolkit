use std::process::ExitCode;

fn main() -> ExitCode {
    let lookup = |name: &str| std::env::var(name).ok();
    let code = api_toolkit::cli::run(
        std::env::args_os(),
        &lookup,
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    );
    ExitCode::from(code)
}
