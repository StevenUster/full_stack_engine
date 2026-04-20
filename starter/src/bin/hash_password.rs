use full_stack_engine::auth::hash_password;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <password>", args[0]);
        std::process::exit(1);
    }
    
    let password = &args[1];
    match hash_password(password) {
        Ok(hash) => println!("{}", hash),
        Err(e) => {
            eprintln!("Error hashing password: {}", e);
            std::process::exit(1);
        }
    }
}
