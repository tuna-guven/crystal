//! Crypto boundary: the age identity and encrypt/decrypt of the vault blob.
//! Plaintext above this module, ciphertext below.

use rustix::io::Errno;
use rustix::rand::{GetRandomFlags, getrandom};

/// Fills `buf` from the kernel's cryptographically secure generator (the
/// getrandom syscall, through rustix). No flags: it blocks until the pool
/// is seeded at boot instead of returning weak bytes, and never reads
/// /dev/urandom, so it needs no file access inside the sandbox. Retries on
/// EINTR and on short reads.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "random ids (#10, #14) and atomic_write temp names"
    )
)]
pub fn fill_random(buf: &mut [u8]) -> Result<(), Errno> {
    let mut filled = 0;
    while filled < buf.len() {
        match getrandom(&mut buf[filled..], GetRandomFlags::empty()) {
            Ok(n) => filled += n,
            Err(Errno::INTR) => {}
            Err(errno) => return Err(errno),
        }
    }
    Ok(())
}

/// Bytes produced by age encryption. The field is private and the only
/// constructors live in this module, so code outside `crypto` cannot label
/// arbitrary bytes as ciphertext and hand them to `vault::atomic_write`.
#[cfg_attr(not(test), expect(dead_code, reason = "built by age encryption (#8)"))]
pub struct Ciphertext(Vec<u8>);

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "written by the save path (#10, #14)")
)]
impl Ciphertext {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Tests in other modules need ciphertext before age encryption exists
    /// (#8). Test builds only: the real program has no way in.
    #[cfg(test)]
    pub fn for_test(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_random_fills_and_differs() {
        let (mut a, mut b) = ([0u8; 32], [0u8; 32]);
        fill_random(&mut a).unwrap();
        fill_random(&mut b).unwrap();
        // 2^-256 chance of a false failure for each assertion.
        assert_ne!(a, [0u8; 32]);
        assert_ne!(a, b);
        fill_random(&mut []).unwrap();
    }
}
