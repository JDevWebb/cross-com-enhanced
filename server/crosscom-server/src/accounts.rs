//! Player accounts, in SQLite.
//!
//! **Players have no passwords.** A player's account belongs to their identity
//! (`crosscom_identity`): an Ed25519 key their launcher keeps, whose public half is the account's
//! global id. The launcher makes the account by signing its name ([`Accounts::add_identity`], from
//! `signup`) and signs in by signing the server's name and the time (`api`'s key login, which
//! [`Accounts::use_key_login`] lets through once per signature). Accounts made by the server's
//! operator (`crosscom-server add-account`: test players, bots) have a password instead, kept as
//! an Argon2 hash; an identity's account has none (an empty hash, which nothing matches).
//!
//! **Game tokens.** The game sends its sign-in secret inside the Quazal protocol, whose encryption
//! uses a key every copy of the game shares: anyone who can see a player's traffic can read it. So
//! the launcher doesn't give the game a password: it signs in over HTTPS with the player's
//! identity (`/api/key-login`), gets a game token (`cct_` and 28 random letters and digits,
//! about 166 bits), and writes that into the game's settings instead. The game signs in with the
//! token (`verify` takes a token or the password); a token seen on the wire is good only for
//! this game on this server, and the player's password, which they may use elsewhere, never leaves
//! the launcher. Tokens are kept as SHA-256 hashes (they're random, so a fast hash is enough), at
//! most `MAX_TOKENS` per account (the oldest go).

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
use sha2::Digest as _;

/// The first player PID; lower ones are left for the server's own principals.
const FIRST_PID: u32 = 0x0010_0000;
/// The longest name; the game's sign-in fields hold 79 characters.
pub const MAX_NAME_LEN: usize = 32;
/// The longest password the game can send.
pub const MAX_PASSWORD_LEN: usize = 79;
/// What every game token starts with: the launcher tells a token from a password by it.
pub const TOKEN_PREFIX: &str = "cct_";
/// The random letters and digits after the prefix.
const TOKEN_RANDOM_LEN: usize = 28;
/// The most game tokens an account keeps: one per PC it plays from, and then some.
const MAX_TOKENS: usize = 10;

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
    /// The identity has an account already (its name).
    IdentityTaken(String),
    InvalidIdentity,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Db(e) => write!(f, "database: {e}"),
            Error::NameTaken => write!(f, "that name is taken"),
            Error::InvalidName => write!(f, "names are 1 to {MAX_NAME_LEN} letters, digits, '-', '_' or '.'"),
            Error::InvalidPassword => write!(f, "passwords are 1 to {MAX_PASSWORD_LEN} characters"),
            Error::Hash => write!(f, "couldn't hash the password"),
            Error::IdentityTaken(name) => write!(f, "this identity has an account here already: {name}"),
            Error::InvalidIdentity => write!(f, "that isn't an identity"),
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
            );
            CREATE TABLE IF NOT EXISTS game_tokens (
                pid INTEGER NOT NULL,
                hash BLOB NOT NULL UNIQUE,
                created INTEGER NOT NULL DEFAULT (unixepoch()),
                last_used INTEGER
            );
            CREATE INDEX IF NOT EXISTS game_tokens_pid ON game_tokens (pid);",
        )?;
        // The identity an account belongs to, and the time of its last key sign-in (each
        // signature goes through once): added to a database from before identities.
        let columns: Vec<String> = {
            let mut query = db.prepare("SELECT name FROM pragma_table_info('accounts')")?;
            let rows = query.query_map([], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        if !columns.iter().any(|c| c == "global_id") {
            db.execute_batch("ALTER TABLE accounts ADD COLUMN global_id TEXT;")?;
        }
        if !columns.iter().any(|c| c == "last_key_login") {
            db.execute_batch("ALTER TABLE accounts ADD COLUMN last_key_login INTEGER NOT NULL DEFAULT 0;")?;
        }
        db.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS accounts_global_id ON accounts (global_id) WHERE global_id IS NOT NULL;")?;
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

    /// Makes the account `name` for the identity `global_id` (no password: the identity signs in);
    /// its PID.
    pub fn add_identity(&self, name: &str, global_id: &str) -> Result<u32, Error> {
        if !valid_name(name) {
            return Err(Error::InvalidName);
        }
        if !crosscom_identity::is_global_id(global_id) {
            return Err(Error::InvalidIdentity);
        }
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let theirs: Option<String> = db.query_row("SELECT name FROM accounts WHERE global_id = ?1", [global_id], |r| r.get(0)).optional()?;
        if let Some(name) = theirs {
            return Err(Error::IdentityTaken(name));
        }
        if db.query_row("SELECT 1 FROM accounts WHERE name = ?1", [name], |_| Ok(())).optional()?.is_some() {
            return Err(Error::NameTaken);
        }
        let next: u32 = db.query_row("SELECT COALESCE(MAX(pid) + 1, ?1) FROM accounts", [FIRST_PID], |r| r.get(0))?;
        db.execute(
            "INSERT INTO accounts (pid, name, password_hash, global_id) VALUES (?1, ?2, '', ?3)",
            params![next, name, global_id],
        )?;
        Ok(next)
    }

    /// The account belonging to the identity `global_id`: its PID and name.
    pub fn by_identity(&self, global_id: &str) -> Result<Option<(u32, String)>, Error> {
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(db
            .query_row("SELECT pid, name FROM accounts WHERE global_id = ?1", [global_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
    }

    /// Notes a key sign-in to `pid` signed at `time`: false (and nothing changed) unless it's
    /// later than the last one, so a signature, seen once, can't sign anyone in again.
    pub fn use_key_login(&self, pid: u32, time: i64) -> Result<bool, Error> {
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(db.execute("UPDATE accounts SET last_key_login = ?2 WHERE pid = ?1 AND last_key_login < ?2", params![pid, time])? == 1)
    }

    /// The name of the account with this PID.
    pub fn name(&self, pid: u32) -> Result<Option<String>, Error> {
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(db.query_row("SELECT name FROM accounts WHERE pid = ?1", [pid], |r| r.get(0)).optional()?)
    }

    /// The PID of the account with this name (in any case).
    pub fn pid(&self, name: &str) -> Result<Option<u32>, Error> {
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(db.query_row("SELECT pid FROM accounts WHERE name = ?1", [name], |r| r.get(0)).optional()?)
    }

    /// Accounts whose names start with `part` (in any case), up to `limit`, by name.
    pub fn search(&self, part: &str, limit: u32) -> Result<Vec<(u32, String)>, Error> {
        let escaped: String = part.chars().flat_map(|c| if matches!(c, '%' | '_' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut query = db.prepare_cached("SELECT pid, name FROM accounts WHERE name LIKE ?1 ESCAPE '\\' ORDER BY name LIMIT ?2")?;
        let rows = query.query_map(params![format!("{escaped}%"), limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The account's PID, if the name and the game's secret match one: one of its game tokens, or
    /// its password. What the game's own sign-in checks (`ticket`).
    pub fn verify(&self, name: &str, secret: &str) -> Result<Option<u32>, Error> {
        if let Some(pid) = self.verify_token(name, secret)? {
            return Ok(Some(pid));
        }
        self.verify_password(name, secret)
    }

    /// The account named `name`'s PID, if `token` is one of its game tokens: proof that a report
    /// (`reach`) is the player's own.
    pub fn token_owner(&self, name: &str, token: &str) -> Result<Option<u32>, Error> {
        self.verify_token(name, token)
    }

    /// The account's PID, if `token` is one of its game tokens.
    fn verify_token(&self, name: &str, token: &str) -> Result<Option<u32>, Error> {
        if !is_token(token) {
            return Ok(None);
        }
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let pid: Option<u32> = db
            .query_row(
                "SELECT accounts.pid FROM accounts JOIN game_tokens ON game_tokens.pid = accounts.pid
                 WHERE accounts.name = ?1 AND game_tokens.hash = ?2",
                params![name, token_hash(token).as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        if pid.is_some() {
            db.execute("UPDATE game_tokens SET last_used = unixepoch() WHERE hash = ?1", [token_hash(token).as_slice()])?;
        }
        Ok(pid)
    }

    /// A new game token for the account `pid` (see the module's notes); the oldest beyond
    /// `MAX_TOKENS` go.
    pub fn issue_token(&self, pid: u32) -> Result<String, Error> {
        const LETTERS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let mut token = String::from(TOKEN_PREFIX);
        while token.len() < TOKEN_PREFIX.len() + TOKEN_RANDOM_LEN {
            let mut byte = [0u8; 1];
            rand::rngs::OsRng.try_fill_bytes(&mut byte).map_err(|_| Error::Hash)?;
            // 248 = 4 * 62: anything above is skipped, so every letter is as likely.
            if usize::from(byte[0]) < 4 * LETTERS.len() {
                token.push(char::from(LETTERS[usize::from(byte[0]) % LETTERS.len()]));
            }
        }
        let db = self.db.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        db.execute("INSERT INTO game_tokens (pid, hash) VALUES (?1, ?2)", params![pid, token_hash(&token).as_slice()])?;
        db.execute(
            "DELETE FROM game_tokens WHERE pid = ?1 AND rowid NOT IN
               (SELECT rowid FROM game_tokens WHERE pid = ?1 ORDER BY rowid DESC LIMIT ?2)",
            params![pid, MAX_TOKENS],
        )?;
        Ok(token)
    }

    /// The account's PID, if the name and password match one: the launcher's sign-in, which
    /// takes the password only (a token can't make more tokens). An unknown name takes as long as
    /// a wrong password, so the time taken doesn't tell which names exist.
    pub fn verify_password(&self, name: &str, password: &str) -> Result<Option<u32>, Error> {
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

/// Whether `secret` has a game token's form.
#[must_use]
pub fn is_token(secret: &str) -> bool {
    secret
        .strip_prefix(TOKEN_PREFIX)
        .is_some_and(|rest| rest.len() == TOKEN_RANDOM_LEN && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn token_hash(token: &str) -> [u8; 32] {
    sha2::Sha256::digest(token.as_bytes()).into()
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
    fn the_game_signs_in_with_a_token_the_launcher_with_the_password_only() {
        let accounts = Accounts::in_memory().unwrap();
        let kozak = accounts.add("Kozak", "s3cret").unwrap();
        let other = accounts.add("Housemate", "pass").unwrap();
        let token = accounts.issue_token(kozak).unwrap();
        assert!(is_token(&token), "{token}");
        assert_eq!(token.len(), 32, "fits the game's fields");
        assert_eq!(accounts.verify("Kozak", &token).unwrap(), Some(kozak), "the game, with the token");
        assert_eq!(accounts.verify("kozak", &token).unwrap(), Some(kozak), "names in any case");
        assert_eq!(accounts.verify("Kozak", "s3cret").unwrap(), Some(kozak), "the game, with the password still");
        assert_eq!(accounts.verify("Housemate", &token).unwrap(), None, "another account's token");
        assert_eq!(accounts.verify_password("Kozak", &token).unwrap(), None, "a token can't sign the launcher in");
        assert_eq!(accounts.verify_password("Kozak", "s3cret").unwrap(), Some(kozak));
        let wrong = format!("{TOKEN_PREFIX}{}", "A".repeat(TOKEN_RANDOM_LEN));
        assert_eq!(accounts.verify("Kozak", &wrong).unwrap(), None, "a token never issued");
        let _ = other;
    }

    #[test]
    fn tokens_are_random_and_only_the_newest_are_kept() {
        let accounts = Accounts::in_memory().unwrap();
        let pid = accounts.add("Kozak", "s3cret").unwrap();
        let first = accounts.issue_token(pid).unwrap();
        let rest: Vec<String> = (0..MAX_TOKENS).map(|_| accounts.issue_token(pid).unwrap()).collect();
        assert!(rest.iter().all(|t| *t != first));
        assert_eq!(accounts.verify("Kozak", &first).unwrap(), None, "the oldest went");
        assert!(rest.iter().all(|t| accounts.verify("Kozak", t).unwrap() == Some(pid)));
        assert!(!is_token("cct_short"));
        assert!(!is_token(&format!("cct_{}!", "a".repeat(TOKEN_RANDOM_LEN - 1))));
    }

    #[test]
    fn pids_count_up_and_names_are_unique() {
        let accounts = Accounts::in_memory().unwrap();
        assert_eq!(accounts.add("Kozak", "a").unwrap(), FIRST_PID);
        assert_eq!(accounts.add("Pepper", "b").unwrap(), FIRST_PID + 1);
        assert!(matches!(accounts.add("PEPPER", "c"), Err(Error::NameTaken)));
    }

    #[test]
    fn finds_players_by_name() {
        let accounts = Accounts::in_memory().unwrap();
        let kozak = accounts.add("Kozak", "a").unwrap();
        accounts.add("Kozlov", "a").unwrap();
        accounts.add("Pepper", "a").unwrap();
        accounts.add("ko_z", "a").unwrap();
        assert_eq!(accounts.pid("KOZAK").unwrap(), Some(kozak));
        assert_eq!(accounts.pid("nobody").unwrap(), None);
        let names = |part: &str| accounts.search(part, 10).unwrap().into_iter().map(|(_, n)| n).collect::<Vec<_>>();
        assert_eq!(names("koz"), vec!["Kozak", "Kozlov"]);
        assert_eq!(names("ko_"), vec!["ko_z"], "_ is a letter here, not a wildcard");
        assert_eq!(names("%"), Vec::<String>::new());
        assert_eq!(accounts.search("", 2).unwrap().len(), 2);
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
