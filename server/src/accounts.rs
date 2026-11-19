//! Pilot accounts: one per callsign (case-insensitive), kept in memory and
//! snapshotted to a JSON file.
//!
//! Passwords: only an argon2id PHC string (salted, slow) is stored. The
//! cleartext arrives in the login message, is checked or hashed here, and is
//! dropped. It is never logged or written anywhere (CLAUDE.md "Credentials").
//!
//! A callsign with no account is provisioned on first login: Quantar, docked
//! at Quantar Core, in a Storm with a starter loadout and 200,000 credits.

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use jumpgate_common::character::{Character, Fittings};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const SNAPSHOT_EVERY: Duration = Duration::from_secs(60);

/// New pilots: Quantar (faction 3) at Quantar-Core Station (id 7, sector 107).
pub const START_FACTION: u8 = 3;
pub const START_BASE: u16 = 7;
pub const START_SHIP: u8 = 90; // Storm
pub const START_CREDITS: u64 = 200_000;

/// Starter Storm loadout, all size 1 (category indices, extract order):
/// engine 0 Money ×2 mounts, powerplant 0 Contender (2.0M ≥ 2 × 758k / 0.8),
/// radar 1 Root, shield 1 Pod, capacitor 0 Mantra. To check in the client's
/// fitting screen.
pub fn starter_fittings() -> Fittings {
    Fittings { engine: 0, powerplant: 0, radar: 1, shield: 1, capacitor: 0, ..Fittings::default() }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    /// argon2id PHC string. Never the password.
    pub password_hash: String,
    pub pilot: Character,
    /// Current location (station id when docked, sector id in space).
    pub loc: u16,
    /// Last station docked at; logins start here.
    pub last_base: u16,
}

#[derive(Serialize, Deserialize, Default)]
struct Snapshot {
    accounts: BTreeMap<String, Account>,
}

pub enum LoginResult {
    Existing,
    Provisioned,
    WrongPassword,
}

pub struct Accounts {
    path: Option<PathBuf>,
    map: BTreeMap<String, Account>,
    dirty: bool,
    last_save: Instant,
    argon: Argon2<'static>,
}

fn key(callsign: &str) -> String {
    callsign.to_ascii_lowercase()
}

impl Accounts {
    /// In memory only (tests), with cheap hashing parameters.
    pub fn in_memory() -> Self {
        let params = Params::new(8, 1, 1, None).unwrap();
        Accounts {
            path: None,
            map: BTreeMap::new(),
            dirty: false,
            last_save: Instant::now(),
            argon: Argon2::new(Algorithm::Argon2id, Version::V0x13, params),
        }
    }

    /// Load `path` if it exists (else start empty); snapshots go back to it.
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let map = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<Snapshot>(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?.accounts,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Accounts { path: Some(path), map, dirty: false, last_save: Instant::now(), argon: Argon2::default() })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Check or create the account for `callsign`. The password is only hashed
    /// or verified, never kept.
    pub fn login(&mut self, callsign: &str, password: &[u8], start_base: u16) -> LoginResult {
        if let Some(a) = self.map.get(&key(callsign)) {
            let ok = PasswordHash::new(&a.password_hash).is_ok_and(|h| self.argon.verify_password(password, &h).is_ok());
            return if ok { LoginResult::Existing } else { LoginResult::WrongPassword };
        }
        let salt = SaltString::generate(&mut OsRng);
        let Ok(hash) = self.argon.hash_password(password, &salt) else {
            return LoginResult::WrongPassword;
        };
        let pilot = Character {
            callsign: callsign.to_string(),
            faction: START_FACTION,
            rank: 0,
            ship_type: START_SHIP,
            fittings: starter_fittings(),
            items: Vec::new(),
            credits: START_CREDITS,
            experience: 0,
        };
        let acct = Account { password_hash: hash.to_string(), pilot, loc: start_base, last_base: start_base };
        self.map.insert(key(callsign), acct);
        self.dirty = true;
        LoginResult::Provisioned
    }

    pub fn get(&self, callsign: &str) -> Option<&Account> {
        self.map.get(&key(callsign))
    }

    /// Mutable access; marks the snapshot dirty.
    pub fn get_mut(&mut self, callsign: &str) -> Option<&mut Account> {
        self.dirty = true;
        self.map.get_mut(&key(callsign))
    }

    /// Write the snapshot if anything changed and a minute has passed.
    pub fn save_if_due(&mut self, now: Instant) -> io::Result<()> {
        if self.dirty && now.duration_since(self.last_save) >= SNAPSHOT_EVERY {
            self.save_now()?;
        }
        Ok(())
    }

    /// Write the snapshot now (temp file + rename, so a crash can't truncate it).
    pub fn save_now(&mut self) -> io::Result<()> {
        self.last_save = Instant::now();
        let Some(path) = &self.path else {
            self.dirty = false;
            return Ok(());
        };
        let snap = Snapshot { accounts: self.map.clone() };
        let text = serde_json::to_string_pretty(&snap).map_err(io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_then_verify() {
        let mut a = Accounts::in_memory();
        assert!(matches!(a.login("Pilot", b"secret", START_BASE), LoginResult::Provisioned));
        assert!(matches!(a.login("pilot", b"secret", START_BASE), LoginResult::Existing), "case-insensitive");
        assert!(matches!(a.login("PILOT", b"wrong", START_BASE), LoginResult::WrongPassword));
        let acct = a.get("Pilot").unwrap();
        assert!(acct.password_hash.starts_with("$argon2id$"));
        assert!(!acct.password_hash.contains("secret"));
        assert_eq!((acct.pilot.ship_type, acct.pilot.credits, acct.pilot.faction, acct.last_base), (90, 200_000, 3, 7));
    }

    #[test]
    fn snapshot_round_trip() {
        let dir = std::env::temp_dir().join(format!("jg-accounts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("accounts.json");
        let _ = std::fs::remove_file(&path);
        {
            let mut a = Accounts::open(path.clone()).unwrap();
            a.argon = Accounts::in_memory().argon;
            a.login("Pilot", b"pw", START_BASE);
            a.get_mut("Pilot").unwrap().pilot.credits = 1234;
            a.save_now().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\"pw\""));
        let mut a = Accounts::open(path.clone()).unwrap();
        a.argon = Accounts::in_memory().argon;
        assert_eq!(a.len(), 1);
        assert_eq!(a.get("pilot").unwrap().pilot.credits, 1234);
        assert!(matches!(a.login("Pilot", b"pw", START_BASE), LoginResult::Existing));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
