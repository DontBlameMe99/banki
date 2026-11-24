use std::env;
use std::error::Error;
use std::path::Path;

fn main() -> Result<(), Box<dyn Error>> {
    for input in ["noctalia.toml", "sync.py", "LICENSE", "assets"] {
        println!("cargo:rerun-if-changed={input}");
    }
    let out = env::var("OUT_DIR")?;
    noctalia_rs_build::generate_to(Path::new("noctalia.toml"), Path::new(&out))
}
