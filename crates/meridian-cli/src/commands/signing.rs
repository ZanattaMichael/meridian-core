//! `meridian signing` and the trust settings every command starts by
//! settling: which keys are trusted, and what to do about a plugin or a
//! module that is not signed.

use crate::*;

/// The trusted keys the command line named, kept for the whole run: plugins
/// and modules are checked against the same ones.
pub(crate) static TRUSTED_KEYS: OnceLock<TrustedKeys> = OnceLock::new();
/// The policy and keys fetched modules are checked against.
pub(crate) static MODULE_SIGNATURES: OnceLock<meridian_module::Verification> = OnceLock::new();

/// Every key to trust: the release's own, then the ones named.
pub(crate) fn trusted_keys() -> Result<Vec<PublicKey>, Failure> {
    match TRUSTED_KEYS.get() {
        Some(keys) => keys.read(),
        None => TrustedKeys::default().read(),
    }
}

/// Settles the trust settings once, before any command runs.
pub(crate) fn settle(trust: Trust) -> Result<(), Failure> {
    let policy = trust.module_signatures;
    let _ = TRUSTED_KEYS.set(trust.keys);
    // With signatures off the keys are not needed, and a key file that has
    // gone missing should not stop a run that does not use it.
    let trusted = match policy {
        SignaturePolicy::Off => Vec::new(),
        SignaturePolicy::Warn | SignaturePolicy::Require => trusted_keys()?,
    };
    let _ = MODULE_SIGNATURES.set(meridian_module::Verification { policy, trusted });
    Ok(())
}

pub(crate) fn signing(command: SigningCommand) -> Result<(), Failure> {
    match command {
        SigningCommand::Keygen { secret, public } => {
            // Neither file is replaced: writing over a secret key loses the
            // only thing that can make signatures its public key accepts.
            for path in [&secret, &public] {
                if path.exists() {
                    return Err(Failure::Message(format!(
                        "{} already exists; a key is not written over",
                        path.display()
                    )));
                }
            }
            let key = SecretKey::generate().map_err(|e| Failure::Message(e.into()))?;
            write_secret(&secret, &format!("{}\n", key.to_text()))
                .and_then(|()| std::fs::write(&public, format!("{}\n", key.public().to_text())))
                .map_err(|e| Failure::Message(format!("writing the key pair: {e}")))?;
            outln!("key {}", key.public().id());
            outln!("  secret: {}", secret.display());
            outln!("  public: {}", public.display());
            Ok(())
        }
        SigningCommand::Sign { key, binaries } => {
            let key = SecretKey::read(&key).map_err(|e| Failure::Message(e.into()))?;
            for binary in binaries {
                let text = key
                    .sign_file(&binary)
                    .map_err(|e| Failure::Message(e.into()))?;
                let path = signature_path(&binary);
                std::fs::write(&path, text)
                    .map_err(|e| Failure::Message(format!("writing {}: {e}", path.display())))?;
                outln!("signed {} with key {}", binary.display(), key.public().id());
            }
            Ok(())
        }
        SigningCommand::Verify { binaries } => {
            let trusted = trusted_keys()?;
            let mut failed = false;
            for binary in binaries {
                let found = meridian_plugin_host::signature::verify_file(&binary, &trusted);
                failed |= !matches!(found, Signature::Verified { .. });
                outln!("{}: {found}", binary.display());
            }
            if failed {
                Err(Failure::Reported)
            } else {
                Ok(())
            }
        }
    }
}

/// Writes a secret key so that only its owner can read it, where the system
/// has such a thing as a file mode.
pub(crate) fn write_secret(path: &Path, text: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(text.as_bytes())
}
