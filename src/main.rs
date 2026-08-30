use clap::Parser;

fn main() {
    let code = match rig::cli::run(rig::cli::Cli::parse()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e:#}");
            2
        }
    };
    std::process::exit(code);
}
