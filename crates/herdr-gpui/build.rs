mod build_identity;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_identity.rs");
    println!("cargo:rerun-if-env-changed=HERDR_BUILD_PR_NUMBER");
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").ok_or("missing manifest directory")?,
    );
    let identity = build_identity::detect(&manifest);
    for path in &identity.watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let pr = match std::env::var("HERDR_BUILD_PR_NUMBER") {
        Ok(value) => build_identity::validate_pr(&value)
            .ok_or("HERDR_BUILD_PR_NUMBER must be empty or a positive decimal integer")?
            .to_owned(),
        Err(std::env::VarError::NotPresent) if identity.worktree && !identity.detached => {
            build_identity::lookup_pr(
                &manifest,
                &identity.branch,
                std::path::Path::new("gh"),
                std::time::Duration::from_secs(2),
            )
            .unwrap_or_default()
        }
        Err(std::env::VarError::NotPresent) => String::new(),
        Err(error) => return Err(error.into()),
    };
    println!(
        "cargo:rustc-env=HERDR_BUILD_WORKTREE={}",
        u8::from(identity.worktree)
    );
    println!("cargo:rustc-env=HERDR_BUILD_BRANCH={}", identity.branch);
    println!("cargo:rustc-env=HERDR_BUILD_PR={pr}");
    Ok(())
}
