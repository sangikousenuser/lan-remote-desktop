pub mod audio;
pub mod channel;
pub mod clipboard;
pub mod color;
pub mod file_worker;
pub mod h264;
pub mod identity;
pub mod network;
pub mod quality;
pub mod resume;
pub mod tls;
pub mod transfer;
pub mod updates;
pub mod wire;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn fail<T>(message: &str) -> Result<T> {
    Err(std::io::Error::other(message).into())
}
pub fn random_u64() -> Result<u64> {
    let mut b = [0u8; 8];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut b)
        .map_err(|_| std::io::Error::other("OS random generator failed"))?;
    Ok(u64::from_be_bytes(b).max(1))
}
