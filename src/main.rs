use rust_template::add;

// print_stdout is denied crate-wide; the entry point is the one place
// intended to print.
#[expect(clippy::print_stdout)]
fn main() {
    println!("2 + 2 = {}", add(2, 2));
}
