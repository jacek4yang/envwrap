fn main() {
    std::process::exit(envwrap::cli::dispatch(std::env::args_os().skip(1)));
}
