// Rebuild when a migration is added, so `sqlx::migrate!` never embeds a stale set.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
