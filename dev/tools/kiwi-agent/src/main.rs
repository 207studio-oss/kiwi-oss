mod board;
mod finding;
mod finish;
mod git_command;
mod sensitive_paths;
mod source;
mod source_time;

fn main() {
    let mut args = std::env::args().skip(1);
    let code = if args.next().as_deref() == Some("finish") {
        finish::main(args)
    } else {
        finish::usage()
    };
    std::process::exit(code);
}
