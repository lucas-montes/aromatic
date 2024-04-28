use aromatic::run_cli;
use clap::Parser;
use console::style;
use std::process;

// cargo invokes this binary as `cargo-sqlx sqlx <args>`
// so the parser below is defined with that in mind
#[derive(Parser, Debug)]
#[clap(bin_name = "cargo")]
enum Cli {
    Sqlx(Opt),
}

#[tokio::main]
async fn main() {
    menva::read_env_file(".env");
    run_cli();
}
