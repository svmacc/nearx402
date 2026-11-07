//! What the wallet keeps on disk, so that finance and the agent can be separate
//! runs, and so that a crash cannot lose the state behind the chain's commitment.
//!
//! The chain only stores a commitment to the spending state. If the wallet forgets
//! the state that commitment opens to, it can never prove again. So the order is:
//!
//! 1. [`WalletFile::begin`] durably records the state a payment would move to.
//! 2. The agent sends the transaction.
//! 3. [`WalletFile::reconcile`] asks what the chain now holds, and adopts it.
//!
//! The chain decides which state is current, never the local outcome of a send: a
//! timeout may still land, and a crash can happen at any step. An intent is kept
//! until a later one is confirmed, because a transaction that looked lost can
//! still land after a newer one was prepared.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{BufReader, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, ensure, Context, Result};
use ark_bn254::{Bn254, Fr};
use ark_ff::PrimeField;
use ark_groth16::ProvingKey;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::fr_le;
use crate::policy::{PolicyOpening, SpendState, ZkWallet};

const VERSION: u32 = 1;
/// Unconfirmed intents kept. A send that lands after this many newer attempts
/// needs recovery through `set_zk_agent`; funds stay safe either way.
const MAX_INTENTS: usize = 32;
/// Temporary names tried per save. One free name is enough, and only leftovers
/// from crashed processes with the same pid can take any.
const MAX_TEMP_ATTEMPTS: u32 = 64;

/// A field element as base64 of 32 little-endian bytes. Rejects anything but the
/// canonical encoding, so one value has only one spelling.
pub fn fr_from_base64(s: &str) -> Result<Fr> {
    let bytes = B64.decode(s).context("field element is not base64")?;
    ensure!(bytes.len() == 32, "field element must be 32 bytes");
    let fr = Fr::from_le_bytes_mod_order(&bytes);
    ensure!(
        fr_le(&fr).as_slice() == bytes,
        "field element is not canonical"
    );
    Ok(fr)
}

fn fr_to_base64(fr: &Fr) -> String {
    B64.encode(fr_le(fr))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateJson {
    day: u64,
    spent: String,
    salt: String,
}

impl From<&SpendState> for StateJson {
    fn from(s: &SpendState) -> Self {
        Self {
            day: s.day,
            spent: s.spent.to_string(),
            salt: fr_to_base64(&s.salt),
        }
    }
}

impl TryFrom<&StateJson> for SpendState {
    type Error = anyhow::Error;
    fn try_from(s: &StateJson) -> Result<Self> {
        Ok(Self {
            day: s.day,
            spent: s.spent.parse().context("spent is not a number")?,
            salt: fr_from_base64(&s.salt).context("state salt")?,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WalletJson {
    version: u32,
    contract: String,
    agent: String,
    cap: String,
    per_call: String,
    vendors: Vec<String>,
    policy_salt: String,
    state: StateJson,
    intents: Vec<StateJson>,
}

/// What the chain holds for this agent.
#[derive(Clone, Copy, Debug)]
pub struct ChainAgent {
    pub policy_commitment: Fr,
    pub state_commitment: Fr,
    /// A token transfer has not settled, so the state may yet be restored.
    pub in_flight: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciled {
    /// The chain holds the state the file already called current.
    Current,
    /// The chain moved to a recorded intent, which is now the current state.
    Adopted,
}

/// The wallet, backed by a file that only one process may hold at a time.
pub struct WalletFile {
    path: PathBuf,
    pub wallet: ZkWallet,
    intents: Vec<SpendState>,
    _lock: File,
}

impl WalletFile {
    /// Saves a new wallet. An existing file holds salts that cannot be recreated,
    /// so this refuses to replace one.
    pub fn create(path: &Path, wallet: ZkWallet) -> Result<Self> {
        let lock = take_lock(path)?;
        ensure_not_a_symlink(path)?;
        ensure!(
            !path.exists(),
            "{} already exists; it holds salts that cannot be recreated",
            path.display()
        );
        let file = Self {
            path: path.to_path_buf(),
            wallet,
            intents: Vec::new(),
            _lock: lock,
        };
        file.save()?;
        Ok(file)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let lock = take_lock(path)?;
        ensure_not_a_symlink(path)?;
        let text = fs::read_to_string(path)
            .with_context(|| format!("read wallet file {}", path.display()))?;
        let j: WalletJson = serde_json::from_str(&text)
            .with_context(|| format!("parse wallet file {}", path.display()))?;
        ensure!(
            j.version == VERSION,
            "wallet file version {} is not supported",
            j.version
        );
        Ok(Self {
            path: path.to_path_buf(),
            wallet: ZkWallet {
                contract: j.contract,
                agent: j.agent,
                opening: PolicyOpening {
                    cap: j.cap.parse().context("cap is not a number")?,
                    per_call: j.per_call.parse().context("per_call is not a number")?,
                    vendors: j.vendors,
                    salt: fr_from_base64(&j.policy_salt).context("policy salt")?,
                },
                state: (&j.state).try_into()?,
            },
            intents: j
                .intents
                .iter()
                .map(SpendState::try_from)
                .collect::<Result<_>>()?,
            _lock: lock,
        })
    }

    /// Durably records the state a payment would move to. Send the transaction
    /// only after this returns.
    pub fn begin(&mut self, next: SpendState) -> Result<()> {
        self.intents.push(next);
        if self.intents.len() > MAX_INTENTS {
            self.intents.remove(0);
        }
        self.save()
    }

    /// Makes the file agree with the chain, or says why it cannot.
    pub fn reconcile(&mut self, chain: &ChainAgent) -> Result<Reconciled> {
        ensure!(
            !chain.in_flight,
            "a payment is still in flight; run again once it has settled"
        );
        ensure!(
            self.wallet.opening.commitment()? == chain.policy_commitment,
            "this wallet file holds a different policy from the contract; \
             finance must send set_zk_agent, or use the right file"
        );
        if self.wallet.state.commitment() == chain.state_commitment {
            return Ok(Reconciled::Current);
        }
        let Some(found) = self
            .intents
            .iter()
            .find(|s| s.commitment() == chain.state_commitment)
            .copied()
        else {
            bail!(
                "the contract holds a spending state this wallet file does not know, so it \
                 cannot prove. Funds are safe. Recover with `zk-setup recover`, which has \
                 finance send set_zk_agent"
            );
        };
        self.wallet.state = found;
        self.intents.clear();
        self.save()?;
        Ok(Reconciled::Adopted)
    }

    /// Writes to a temporary file and renames it, so a crash leaves either the
    /// old file or the new one, never a partial one.
    fn save(&self) -> Result<()> {
        let w = &self.wallet;
        let json = WalletJson {
            version: VERSION,
            contract: w.contract.clone(),
            agent: w.agent.clone(),
            cap: w.opening.cap.to_string(),
            per_call: w.opening.per_call.to_string(),
            vendors: w.opening.vendors.clone(),
            policy_salt: fr_to_base64(&w.opening.salt),
            state: (&w.state).into(),
            intents: self.intents.iter().map(StateJson::from).collect(),
        };
        write_private(&self.path, &serde_json::to_vec_pretty(&json)?)
    }
}

/// Replaces `path` with `contents`, readable by its owner only on Unix.
///
/// A crash leaves either the old file or the new one, never a partial one. The
/// contents go to a new temporary file beside it, so a leftover file or symlink
/// can neither lend its permissions to the result nor be written through.
///
/// This defends against leftovers, not against another user who can write to the
/// parent directory: they could swap or delete files whatever this does. Keep the
/// directory writable by its owner only.
pub fn write_private(path: &Path, contents: &[u8]) -> Result<()> {
    let (mut file, tmp) = new_temp_file(path)?;
    let replaced = file
        .write_all(contents)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("write {}", tmp.display()))
        .and_then(|()| {
            fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
        });
    if replaced.is_err() {
        // The contents may be private, so do not leave them behind.
        let _ = fs::remove_file(&tmp);
    }
    replaced?;
    File::open(parent(path))
        .and_then(|d| d.sync_all())
        .with_context(|| format!("sync the directory of {}", path.display()))
}

fn temp_name(path: &Path, pid: u32, attempt: u32) -> PathBuf {
    sibling(path, &format!("{pid}.{attempt}.tmp"))
}

/// The name only has to avoid leftovers: `create_new` is what makes this safe,
/// since it fails on anything already there, including a dangling symlink.
fn new_temp_file(path: &Path) -> Result<(File, PathBuf)> {
    for attempt in 0..MAX_TEMP_ATTEMPTS {
        let tmp = temp_name(path, std::process::id(), attempt);
        match private_new_file(&tmp) {
            Ok(file) => return Ok((file, tmp)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).with_context(|| format!("create {}", tmp.display())),
        }
    }
    bail!(
        "no free temporary name beside {} after {MAX_TEMP_ATTEMPTS} attempts; look for leftover \
         .tmp files there",
        path.display()
    )
}

fn private_new_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Saving would replace a symlink with a file, and leave the old contents behind
/// wherever it pointed, so a wallet file has to be a plain file.
fn ensure_not_a_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                !meta.file_type().is_symlink(),
                "{} is a symlink; a wallet file must be a regular file",
                path.display()
            );
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("inspect {}", path.display())),
    }
}

fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

fn sibling(path: &Path, extension: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(extension);
    PathBuf::from(name)
}

/// The lock ends with the process, so a crash cannot leave it stuck.
fn take_lock(path: &Path) -> Result<File> {
    let lock_path = sibling(path, "lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open {}", lock_path.display()))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => {
            bail!("{} is in use by another process", path.display())
        }
        Err(TryLockError::Error(e)) => Err(anyhow!(e).context("lock the wallet file")),
    }
}

/// Saves the proving key. It is not secret, and refuses to replace an existing
/// file, because the contract's verifying key must keep matching it.
pub fn save_proving_key(path: &Path, pk: &ProvingKey<Bn254>) -> Result<()> {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    let mut w = BufWriter::new(file);
    pk.serialize_uncompressed(&mut w)
        .map_err(|e| anyhow!("write the proving key: {e}"))?;
    w.flush()?;
    w.get_ref()
        .sync_all()
        .with_context(|| format!("sync {}", path.display()))
}

/// Loads the proving key, checking every point it holds.
pub fn load_proving_key(path: &Path) -> Result<ProvingKey<Bn254>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    ProvingKey::deserialize_uncompressed(BufReader::new(file))
        .map_err(|e| anyhow!("read the proving key {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::setup;
    use ark_std::rand::SeedableRng;
    use ark_std::UniformRand;
    use rand_chacha::ChaCha20Rng;

    fn rng() -> ChaCha20Rng {
        ChaCha20Rng::seed_from_u64(7)
    }

    struct Dir(PathBuf);
    impl Dir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "zx402-store-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn wallet(&self) -> PathBuf {
            self.0.join("wallet.json")
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn wallet(r: &mut ChaCha20Rng) -> ZkWallet {
        ZkWallet {
            contract: "policy.near".into(),
            agent: "agent.near".into(),
            opening: PolicyOpening {
                cap: 1_000,
                per_call: 500,
                vendors: vec!["a.near".into(), "b.near".into()],
                salt: Fr::rand(r),
            },
            state: SpendState::fresh(r),
        }
    }

    fn next(r: &mut ChaCha20Rng, spent: u128) -> SpendState {
        SpendState {
            day: 20_000,
            spent,
            salt: Fr::rand(r),
        }
    }

    fn chain(file: &WalletFile, state: Fr) -> ChainAgent {
        ChainAgent {
            policy_commitment: file.wallet.opening.commitment().unwrap(),
            state_commitment: state,
            in_flight: false,
        }
    }

    #[test]
    fn a_wallet_survives_a_round_trip_through_its_file() {
        let d = Dir::new("round-trip");
        let mut r = rng();
        let w = wallet(&mut r);
        let (policy, state) = (w.opening.commitment().unwrap(), w.state.commitment());
        let intent = next(&mut r, 100);
        let mut f = WalletFile::create(&d.wallet(), w).unwrap();
        f.begin(intent).unwrap();
        drop(f);

        let f = WalletFile::open(&d.wallet()).unwrap();
        assert_eq!(f.wallet.contract, "policy.near");
        assert_eq!(f.wallet.agent, "agent.near");
        assert_eq!(f.wallet.opening.cap, 1_000);
        assert_eq!(f.wallet.opening.per_call, 500);
        assert_eq!(f.wallet.opening.vendors, ["a.near", "b.near"]);
        assert_eq!(f.wallet.opening.commitment().unwrap(), policy);
        assert_eq!(f.wallet.state.commitment(), state);
        assert_eq!(f.intents.len(), 1);
        assert_eq!(f.intents[0].commitment(), intent.commitment());
    }

    #[test]
    fn create_refuses_to_replace_a_wallet() {
        let d = Dir::new("no-replace");
        let mut r = rng();
        drop(WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap());
        let err = WalletFile::create(&d.wallet(), wallet(&mut r))
            .err()
            .unwrap();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn crash_after_the_send_is_recovered_from_the_chain() {
        let d = Dir::new("crash-after");
        let mut r = rng();
        let mut f = WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap();
        let intent = next(&mut r, 100);
        f.begin(intent).unwrap();
        // The transaction landed, and the process died before it could confirm.
        drop(f);

        let mut f = WalletFile::open(&d.wallet()).unwrap();
        let c = chain(&f, intent.commitment());
        assert_eq!(f.reconcile(&c).unwrap(), Reconciled::Adopted);
        assert_eq!(f.wallet.state.commitment(), intent.commitment());
        assert_eq!(f.wallet.state.spent, 100);
        assert!(f.intents.is_empty());
        drop(f);

        // And that was durable.
        let f = WalletFile::open(&d.wallet()).unwrap();
        assert_eq!(f.wallet.state.commitment(), intent.commitment());
    }

    #[test]
    fn crash_before_the_send_keeps_the_current_state() {
        let d = Dir::new("crash-before");
        let mut r = rng();
        let w = wallet(&mut r);
        let state = w.state.commitment();
        let mut f = WalletFile::create(&d.wallet(), w).unwrap();
        f.begin(next(&mut r, 100)).unwrap();
        drop(f);

        let mut f = WalletFile::open(&d.wallet()).unwrap();
        let c = chain(&f, state);
        assert_eq!(f.reconcile(&c).unwrap(), Reconciled::Current);
        assert_eq!(f.wallet.state.commitment(), state);
    }

    #[test]
    fn a_send_that_looked_lost_can_still_land_after_a_newer_intent() {
        let d = Dir::new("late-landing");
        let mut r = rng();
        let w = wallet(&mut r);
        let state = w.state.commitment();
        let mut f = WalletFile::create(&d.wallet(), w).unwrap();
        let first = next(&mut r, 100);
        f.begin(first).unwrap();

        // It looks lost: the chain still holds the old state.
        let c = chain(&f, state);
        assert_eq!(f.reconcile(&c).unwrap(), Reconciled::Current);
        f.begin(next(&mut r, 100)).unwrap();

        // Then the first one lands.
        let c = chain(&f, first.commitment());
        assert_eq!(f.reconcile(&c).unwrap(), Reconciled::Adopted);
        assert_eq!(f.wallet.state.commitment(), first.commitment());
    }

    #[test]
    fn an_unknown_chain_state_is_refused_and_names_the_recovery() {
        let d = Dir::new("unknown");
        let mut r = rng();
        let w = wallet(&mut r);
        let state = w.state.commitment();
        let mut f = WalletFile::create(&d.wallet(), w).unwrap();
        let c = chain(&f, next(&mut r, 1).commitment());
        let err = f.reconcile(&c).unwrap_err().to_string();
        assert!(err.contains("zk-setup recover"), "{err}");
        assert_eq!(f.wallet.state.commitment(), state);
    }

    #[test]
    fn a_payment_in_flight_decides_nothing() {
        let d = Dir::new("in-flight");
        let mut r = rng();
        let w = wallet(&mut r);
        let state = w.state.commitment();
        let mut f = WalletFile::create(&d.wallet(), w).unwrap();
        let intent = next(&mut r, 100);
        f.begin(intent).unwrap();
        // The chain shows the new state, but the transfer can still fail and
        // restore the old one.
        let mut c = chain(&f, intent.commitment());
        c.in_flight = true;
        let err = f.reconcile(&c).unwrap_err().to_string();
        assert!(err.contains("in flight"), "{err}");
        assert_eq!(f.wallet.state.commitment(), state);
        assert_eq!(f.intents.len(), 1);
    }

    #[test]
    fn another_policy_is_refused() {
        let d = Dir::new("other-policy");
        let mut r = rng();
        let w = wallet(&mut r);
        let state = w.state.commitment();
        let mut f = WalletFile::create(&d.wallet(), w).unwrap();
        let mut c = chain(&f, state);
        c.policy_commitment = Fr::rand(&mut r);
        let err = f.reconcile(&c).unwrap_err().to_string();
        assert!(err.contains("different policy"), "{err}");
    }

    #[test]
    fn only_the_newest_intents_are_kept() {
        let d = Dir::new("bounded");
        let mut r = rng();
        let mut f = WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap();
        let oldest = next(&mut r, 1);
        f.begin(oldest).unwrap();
        for i in 0..MAX_INTENTS {
            f.begin(next(&mut r, i as u128 + 2)).unwrap();
        }
        assert_eq!(f.intents.len(), MAX_INTENTS);
        assert!(f
            .intents
            .iter()
            .all(|s| s.commitment() != oldest.commitment()));
    }

    #[test]
    fn a_second_process_cannot_hold_the_wallet() {
        let d = Dir::new("locked");
        let mut r = rng();
        let held = WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap();
        let err = WalletFile::open(&d.wallet()).err().unwrap().to_string();
        assert!(err.contains("in use"), "{err}");
        drop(held);
        assert!(WalletFile::open(&d.wallet()).is_ok());
    }

    #[test]
    fn a_damaged_file_is_refused() {
        let d = Dir::new("damaged");
        let mut r = rng();
        drop(WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap());
        let good = fs::read_to_string(d.wallet()).unwrap();

        fs::write(d.wallet(), &good[..good.len() / 2]).unwrap();
        assert!(WalletFile::open(&d.wallet()).is_err());

        fs::write(d.wallet(), good.replace("\"version\": 1", "\"version\": 2")).unwrap();
        let err = WalletFile::open(&d.wallet()).err().unwrap().to_string();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn field_elements_have_only_one_encoding() {
        let mut r = rng();
        let x = Fr::rand(&mut r);
        assert_eq!(fr_from_base64(&fr_to_base64(&x)).unwrap(), x);
        // All ones is above the modulus, so it would be an alias if accepted.
        assert!(fr_from_base64(&B64.encode([0xff; 32]))
            .unwrap_err()
            .to_string()
            .contains("canonical"));
        assert!(fr_from_base64(&B64.encode([1; 31])).is_err());
        assert!(fr_from_base64("not base64!").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_wallet_file_is_private_after_every_save() {
        use std::os::unix::fs::PermissionsExt;
        let d = Dir::new("private");
        let mut r = rng();
        let mut f = WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap();
        f.begin(next(&mut r, 1)).unwrap();
        let mode = fs::metadata(d.wallet()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Codex's reproduction: `mode(0o600)` only applies to a file it creates.
    #[cfg(unix)]
    #[test]
    fn a_stale_readable_temp_file_does_not_make_the_wallet_readable() {
        use std::os::unix::fs::PermissionsExt;
        let d = Dir::new("stale-temp");
        let mut r = rng();
        let stale = sibling(&d.wallet(), "tmp");
        fs::write(&stale, "left behind").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o644)).unwrap();

        drop(WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap());

        let mode = fs::metadata(d.wallet()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_temp_symlink_is_not_followed() {
        let d = Dir::new("stale-link");
        let mut r = rng();
        let victim = d.0.join("victim");
        fs::write(&victim, "keep me").unwrap();
        std::os::unix::fs::symlink(&victim, sibling(&d.wallet(), "tmp")).unwrap();

        drop(WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap());

        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
        let meta = fs::symlink_metadata(d.wallet()).unwrap();
        assert!(meta.file_type().is_file());
    }

    /// The two tests above pass by the name changing alone. This one puts leftovers
    /// on the names a save actually tries, so it needs `create_new` to pass.
    #[cfg(unix)]
    #[test]
    fn leftovers_on_the_names_a_save_tries_are_skipped_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let d = Dir::new("collisions");
        let mut r = rng();
        let pid = std::process::id();
        let readable = temp_name(&d.wallet(), pid, 0);
        fs::write(&readable, "readable").unwrap();
        fs::set_permissions(&readable, fs::Permissions::from_mode(0o644)).unwrap();
        let victim = d.0.join("victim");
        fs::write(&victim, "keep me").unwrap();
        let link = temp_name(&d.wallet(), pid, 1);
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let dangling = temp_name(&d.wallet(), pid, 2);
        let missing = d.0.join("missing");
        std::os::unix::fs::symlink(&missing, &dangling).unwrap();

        let mut f = WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap();
        f.begin(next(&mut r, 1)).unwrap();

        let meta = fs::symlink_metadata(d.wallet()).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_to_string(&readable).unwrap(), "readable");
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep me");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(fs::symlink_metadata(&dangling).unwrap().is_symlink());
        assert!(!missing.exists(), "a dangling symlink was written through");
        drop(f);
        assert_eq!(WalletFile::open(&d.wallet()).unwrap().intents.len(), 1);
    }

    #[test]
    fn with_no_free_temp_name_the_save_fails_and_the_wallet_survives() {
        let d = Dir::new("no-name");
        let mut r = rng();
        let w = wallet(&mut r);
        let state = w.state.commitment();
        let mut f = WalletFile::create(&d.wallet(), w).unwrap();
        let before = fs::read(d.wallet()).unwrap();
        for attempt in 0..MAX_TEMP_ATTEMPTS {
            fs::write(temp_name(&d.wallet(), std::process::id(), attempt), "").unwrap();
        }
        let err = f.begin(next(&mut r, 1)).unwrap_err().to_string();
        assert!(err.contains("no free temporary name"), "{err}");
        assert_eq!(fs::read(d.wallet()).unwrap(), before);
        assert_eq!(f.wallet.state.commitment(), state);
    }

    #[test]
    fn a_failed_replace_leaves_no_temp_file_behind() {
        let d = Dir::new("failed-rename");
        // A directory that is not empty cannot be replaced by a rename.
        let target = d.0.join("target");
        fs::create_dir_all(target.join("inside")).unwrap();
        assert!(write_private(&target, b"private").is_err());
        let names: Vec<_> = fs::read_dir(&d.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["target"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_readable_wallet_is_private_after_the_next_save() {
        use std::os::unix::fs::PermissionsExt;
        let d = Dir::new("repair");
        let mut r = rng();
        drop(WalletFile::create(&d.wallet(), wallet(&mut r)).unwrap());
        fs::set_permissions(d.wallet(), fs::Permissions::from_mode(0o644)).unwrap();

        let mut f = WalletFile::open(&d.wallet()).unwrap();
        f.begin(next(&mut r, 1)).unwrap();

        let mode = fs::metadata(d.wallet()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_wallet_path_that_is_a_symlink_is_refused() {
        let d = Dir::new("wallet-link");
        let mut r = rng();
        let elsewhere = d.0.join("elsewhere.json");
        drop(WalletFile::create(&elsewhere, wallet(&mut r)).unwrap());
        let before = fs::read(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, d.wallet()).unwrap();

        let err = WalletFile::open(&d.wallet()).err().unwrap().to_string();
        assert!(err.contains("symlink"), "{err}");
        let err = WalletFile::create(&d.wallet(), wallet(&mut r))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("symlink"), "{err}");

        // `exists()` follows links, so a dangling one must be caught here too.
        let dangling = d.0.join("dangling.json");
        std::os::unix::fs::symlink(d.0.join("missing"), &dangling).unwrap();
        let err = WalletFile::create(&dangling, wallet(&mut r))
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("symlink"), "{err}");

        assert_eq!(fs::read(&elsewhere).unwrap(), before);
    }

    #[test]
    fn a_proving_key_round_trips_and_is_not_replaced() {
        let d = Dir::new("pk");
        let mut r = rng();
        let (pk, _) = setup(&mut r).unwrap();
        let path = d.0.join("proving.key");
        save_proving_key(&path, &pk).unwrap();
        assert_eq!(load_proving_key(&path).unwrap(), pk);
        assert!(save_proving_key(&path, &pk).is_err());

        // A truncated key does not load.
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(load_proving_key(&path).is_err());
    }
}
