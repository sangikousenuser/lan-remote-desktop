//! Offline release signer. Private keys are created outside the source checkout
//! and never printed. CI signing can read the same PKCS#8 from a protected secret.
use lrd_session::{
    fail, identity,
    updates::{Entry, Envelope, Manifest, Policy, VerifiedManifest, MAX_PACKAGE},
    Result,
};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde::Deserialize;
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    version: String,
    target: String,
    minimum_os: String,
    package_url: String,
    files: Vec<Member>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    path: String,
    executable: bool,
}

fn bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        return fail("invalid release input");
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return fail("release input exceeded limit");
    }
    Ok(bytes)
}
fn sign(key_path: &Path, recipe_path: &Path, source: &Path, output: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(key_path)?.permissions().mode() & 0o077 != 0 {
            return fail("signing key must be owner-only");
        }
    }
    let key = Ed25519KeyPair::from_pkcs8(&bounded(key_path, 16384)?)
        .map_err(|_| std::io::Error::other("invalid signing key"))?;
    let recipe: Recipe = serde_json::from_slice(&bounded(recipe_path, 128 * 1024)?)?;
    if !matches!(
        recipe.target.as_str(),
        "x86_64-pc-windows-msvc" | "x86_64-apple-darwin" | "aarch64-apple-darwin"
    ) || recipe.files.is_empty()
        || recipe.files.len() > 256
    {
        return fail("release platform/file limits");
    }
    let root = cap_std::fs::Dir::open_ambient_dir(source, cap_std::ambient_authority())?;
    let mut bytes = b"LRDUPD1\n".to_vec();
    let mut files = Vec::new();
    for member in recipe.files {
        lrd_session::transfer::validate_path(&member.path)?;
        let mut parent = std::path::PathBuf::new();
        for part in Path::new(&member.path).components() {
            parent.push(part);
            if root.symlink_metadata(&parent)?.file_type().is_symlink() {
                return fail("release symlink rejected");
            }
        }
        let mut file = root.open(&member.path)?;
        if !file.metadata()?.is_file() {
            return fail("release member is not a file");
        }
        let offset = bytes.len();
        Read::by_ref(&mut file)
            .take((MAX_PACKAGE - offset) as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_PACKAGE {
            return fail("release package limit");
        }
        files.push(Entry {
            path: member.path,
            size: (bytes.len() - offset) as u64,
            sha256: identity::fingerprint(&bytes[offset..]),
            executable: member.executable,
        });
    }
    let manifest = Manifest {
        schema: 1,
        version: recipe.version,
        target: recipe.target,
        minimum_os: recipe.minimum_os,
        protocol_major: 1,
        package_url: recipe.package_url,
        package_size: bytes.len() as u64,
        package_sha256: identity::fingerprint(&bytes),
        files,
    };
    let manifest_text = serde_json::to_string(&manifest)?;
    let envelope = serde_json::to_vec(&Envelope {
        signature: identity::hex(key.sign(manifest_text.as_bytes()).as_ref()),
        manifest: manifest_text,
    })?;
    let public: [u8; 32] = key.public_key().as_ref().try_into()?;
    let policy = Policy {
        installed: "0.0.0",
        highest_staged: None,
        target: &manifest.target,
        os: &manifest.minimum_os,
        public_key: &public,
    };
    VerifiedManifest::verify(&envelope, &policy)?.verify_package(bytes.clone())?;
    fs::create_dir_all(output)?;
    // create_new avoids silently replacing previously reviewed release artifacts.
    let mut package = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join(format!("package-{}.lrdpkg", manifest.target)))?;
    package.write_all(&bytes)?;
    package.sync_all()?;
    let mut metadata = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join(format!("manifest-{}.json", manifest.target)))?;
    metadata.write_all(&envelope)?;
    metadata.sync_all()?;
    println!(
        "Signed {} {} ({} bytes)",
        manifest.version,
        manifest.target,
        bytes.len()
    );
    Ok(())
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("keygen") if args.len()==3 => {
            let private = Path::new(&args[1]); let public = Path::new(&args[2]);
            if private.exists() || public.exists() { return fail("key output already exists"); }
            identity::private_directory(private.parent().ok_or("private key needs a directory")?)?;
            let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| std::io::Error::other("key generation failed"))?;
            let key = Ed25519KeyPair::from_pkcs8(document.as_ref()).map_err(|_| std::io::Error::other("invalid generated key"))?;
            identity::write_private_new(private, document.as_ref())?;
            let mut file = fs::OpenOptions::new().create_new(true).write(true).open(public)?;
            writeln!(file, "{}", identity::hex(key.public_key().as_ref()))?; file.sync_all()?;
            println!("Created private signing key and public trust anchor; existing keys are never overwritten.");
            Ok(())
        }
        Some("sign") if args.len()==5 => sign(Path::new(&args[1]),Path::new(&args[2]),Path::new(&args[3]),Path::new(&args[4])),
        _ => fail("usage: lrd-release-tool keygen PRIVATE_PK8 PUBLIC_HEX | sign PRIVATE_PK8 RECIPE_JSON SOURCE_ROOT OUTPUT_DIRECTORY"),
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Release tool: {e}");
        std::process::exit(1);
    }
}
