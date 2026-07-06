//! The `fse` binary. Implementation lands in build-order step 3:
//! `fse migrate` (parse tables/, diff vs snapshot, write + apply a sqlx
//! migration) and step 5: `fse init` (introspect an existing SQLite database
//! into table structs + snapshot).

fn main() {
    eprintln!("fse: not implemented yet — `fse migrate` and `fse init` land in build-order steps 3 and 5");
    std::process::exit(1);
}
