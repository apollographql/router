fn main() {
    println!("testing");
    match apollo_router::main() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
