use clap::Parser;

mod cli;
mod dispatch;

fn main() {
    let cli = cli::Cli::parse();
    if let Err(e) = dispatch::run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
