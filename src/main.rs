fn main() -> std::process::ExitCode {
    match agent_sessions::cli::run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
