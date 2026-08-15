use mf_lab::corrhist::{Config, run, usage};

fn main() {
    let config = Config::parse_args(std::env::args().skip(1)).unwrap_or_else(|error| {
        eprintln!("error: {error}\n{}", usage());
        std::process::exit(2);
    });
    if let Err(error) = run(&config) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
