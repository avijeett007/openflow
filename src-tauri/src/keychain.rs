//! OS keychain storage for API keys (macOS Keychain / Windows Credential
//! Manager / Linux Secret Service via the `keyring` crate). Keys never touch
//! the settings JSON on disk. Accounts are namespaced `"{scope}:{provider}"`,
//! e.g. `"stt:groq"` or `"cleanup:openai"`, under one service name.

use anyhow::Result;
use keyring::Entry;

const SERVICE: &str = "knotie.ai.openflow";

fn account(scope: &str, provider: &str) -> String {
    format!("{scope}:{provider}")
}

fn entry(scope: &str, provider: &str) -> Result<Entry> {
    Ok(Entry::new(SERVICE, &account(scope, provider))?)
}

/// Store (or overwrite) an API key. An empty value deletes the entry so
/// "cleared in the UI" means "removed from the keychain", not "stored blank".
pub fn set_api_key(scope: &str, provider: &str, key: &str) -> Result<()> {
    let entry = entry(scope, provider)?;
    if key.is_empty() {
        // Deleting a missing entry is not an error for our purposes.
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(key)?;
    Ok(())
}

/// Fetch an API key. Returns `None` when no entry exists (never errors on the
/// common "not set yet" path).
pub fn get_api_key(scope: &str, provider: &str) -> Option<String> {
    match entry(scope, provider) {
        Ok(e) => match e.get_password() {
            Ok(v) => Some(v),
            Err(keyring::Error::NoEntry) => None,
            Err(_) => None,
        },
        Err(_) => None,
    }
}

pub fn has_api_key(scope: &str, provider: &str) -> bool {
    get_api_key(scope, provider)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

pub fn delete_api_key(scope: &str, provider: &str) -> Result<()> {
    if let Ok(e) = entry(scope, provider) {
        let _ = e.delete_credential();
    }
    Ok(())
}
