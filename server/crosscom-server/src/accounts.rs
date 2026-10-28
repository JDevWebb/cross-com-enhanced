//! Player accounts, in SQLite. Passwords are kept as Argon2 hashes.

use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;

use argon2::password_hash::PasswordHash;
use argon2::password_hash::PasswordHasher;
use argon2::password_hash::PasswordVerifier;
use argon2::password_hash::SaltString;
use argon2::Argon2;
use rand::TryRngCore;
use rusqlite::params;
use rusqlite::Connection;
use rusqlite::OptionalExtension;

/// The first player PID; lower ones are left for the server's own principals.
const FIRST_PID: u32 = 0x0010_0000;
/// The longest name; the game's sign-in fields hold 79 characters.
pub const MAX_NAME_LEN: usize = 32;
/// The longest password the game can send.
pub const MAX_PASSWORD_LEN: usize = 79;

pub struct Accounts {
    db: Mutex<Connection>,
}

#[derive(Debug)]
pub enum Error {
    Db(rusqlite::Error),
    NameTaken,
    InvalidName,
    InvalidPassword,
    Hash,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Db(e) => write!(f, "database: {e}"),
            Error::NameTaken => write!(f, "that name is taken"),
            Error::InvalidName => write!(f, "names are 1 to {MAX_NAME_LEN} letters, digits, '-', '_' or '.'"),
            Error::InvalidPassword => write!(f, "passwords are 1 to {MAX_PASSWORD_LEN} characters"),
            Error::Hash => write!(f, "couldn't hash the password"),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

impl Accounts {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::with(Connection::open(path)?)
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self, Error> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> Result<Self, Error> {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS accounts (
                pid INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                password_hash TEXT NOT NULL,
                created INTEGER NOT NULL DEFAULT (unixepoch())
            );",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Creates an account and returns its PID.
    pub fn add(&self, name: &str, password: &str) -> Result<u32, Error> {
        if !valid_name(name) {
            return Err(Error::InvalidName);
        }
        if password.is_empty() || password.chars().count() > MAX_PASSWORD_LEN || password.contains('\0') {
            return Err(Error::InvalidPassword);
        }
        let mut salt = [0u8; 16];
        rand::rngs::OsRng.try_fill_bytes(&mut salt).map_err(|_| Error::Hash)?;
        let salt = SaltString::encode_b64(&salt).map_err(|_| Error::Hash)?;
        let hash = Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|_| Error::Hash)?.to_string();
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if db.query_row("SELECT 1 FROM accounts WHERE name = ?1", [name], |_| Ok(())).optional()?.is_some() {
            return Err(Error::NameTaken);
        }
        let next: u32 = db.query_row("SELECT COALESCE(MAX(pid) + 1, ?1) FROM accounts", [FIRST_PID], |r| r.get(0))?;
        db.execute("INSERT INTO accounts (pid, name, password_hash) VALUES (?1, ?2, ?3)", params![next, name, hash])?;
        Ok(next)
    }

    /// The account's PID, if the name and password match one. An unknown name takes as long as a
    /// wrong password, so the time taken doesn't tell which names exist.
    pub fn verify(&self, name: &str, password: &str) -> Result<Option<u32>, Error> {
        if password.chars().count() > MAX_PASSWORD_LEN {
            return Ok(None);
        }
        let row: Option<(u32, String)> = {
            let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            db.query_row("SELECT pid, password_hash FROM accounts WHERE name = ?1", [name], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
        };
        let Some((pid, hash)) = row else {
            let _ = PasswordHash::new(dummy_hash()).map(|h| Argon2::default().verify_password(password.as_bytes(), &h));
            return Ok(None);
        };
        let Ok(hash) = PasswordHash::new(&hash) else {
            return Ok(None);
        };
        Ok(Argon2::default().verify_password(password.as_bytes(), &hash).is_ok().then_some(pid))
    }
}

/// A hash to check passwords against when the name is unknown.
fn dummy_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        let salt = SaltString::encode_b64(&[0x5a; 16]).expect("a fixed salt");
        Argon2::default().hash_password(b"no such account", &salt).map(|h| h.to_string()).unwrap_or_default()
    })
}

pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME_LEN).contains(&name.chars().count()) && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signs_in_with_the_right_password_only() {
        let accounts = Accounts::in_memory().unwrap();
        let pid = accounts.add("Kozak", "hunter2").unwrap();
        assert_eq!(pid, FIRST_PID);
        assert_eq!(accounts.verify("Kozak", "hunter2").unwrap(), Some(pid));
        assert_eq!(accounts.verify("kozak", "hunter2").unwrap(), Some(pid), "names ignore case");
        assert_eq!(accounts.verify("Kozak", "wrong").unwrap(), None);
        assert_eq!(accounts.verify("Pepper", "hunter2").unwrap(), None);
        assert_eq!(accounts.verify("Kozak", &"x".repeat(MAX_PASSWORD_LEN + 1)).unwrap(), None);
    }

    #[test]
    fn pids_count_up_and_names_are_unique() {
        let accounts = Accounts::in_memory().unwrap();
        assert_eq!(accounts.add("Kozak", "a").unwrap(), FIRST_PID);
        assert_eq!(accounts.add("Pepper", "b").unwrap(), FIRST_PID + 1);
        assert!(matches!(accounts.add("PEPPER", "c"), Err(Error::NameTaken)));
    }

    #[test]
    fn checks_names_and_passwords() {
        let accounts = Accounts::in_memory().unwrap();
        assert!(matches!(accounts.add("", "a"), Err(Error::InvalidName)));
        assert!(matches!(accounts.add("has space", "a"), Err(Error::InvalidName)));
        assert!(matches!(accounts.add("ok", ""), Err(Error::InvalidPassword)));
        assert!(matches!(accounts.add("ok", &"x".repeat(MAX_PASSWORD_LEN + 1)), Err(Error::InvalidPassword)));
    }
}
