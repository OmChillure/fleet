use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Argon2, Algorithm, Params, Version};
use rand::rngs::OsRng;

use crate::error::{AppError, AppResult};

/// Argon2id with moderate memory cost — safe default for interactive login.
fn argon2() -> AppResult<Argon2<'static>> {
    let params = Params::new(19_456, 2, 1, None)
        .map_err(|e| AppError::internal(format!("argon2 params: {e}")))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn hash_password(password: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = argon2()?
        .hash_password(password.as_bytes(), &salt)?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, password_hash: &str) -> AppResult<bool> {
    let parsed = PasswordHash::new(password_hash)
        .map_err(|_| AppError::internal("stored password hash is invalid"))?;
    Ok(argon2()?
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_ok() {
        let hash = hash_password("correct-horse").expect("hash");
        assert!(verify_password("correct-horse", &hash).expect("verify"));
    }

    #[test]
    fn wrong_password_returns_false() {
        let hash = hash_password("correct-horse").expect("hash");
        assert!(!verify_password("wrong-password", &hash).expect("verify"));
    }

    #[test]
    fn garbage_hash_is_error_not_panic() {
        let result = verify_password("anything", "not-a-valid-argon2-hash");
        assert!(result.is_err(), "expected Err, got {result:?}");
    }
}
