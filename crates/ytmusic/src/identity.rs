//! Anonymous visitor-data identity and rotation.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::context::home;
use crate::error::{Error, Result};

/// Google's consent cookie sent with anonymous requests.
pub const CONSENT_COOKIE: &str = "SOCS=CAISAiAD";

#[derive(Default)]
struct Pool {
    ids: Vec<String>,
    banned: HashSet<String>,
}

static POOL: OnceLock<Mutex<Pool>> = OnceLock::new();

fn pool() -> &'static Mutex<Pool> {
    POOL.get_or_init(|| Mutex::new(Pool::default()))
}

/// Lock the pool, recovering the guard if a previous holder panicked.
///
/// The pool is a plain `Vec`/`HashSet` with no invariant a panic can break, so
/// recovering is strictly better than the `.unwrap()` this replaces: a panic
/// elsewhere in the process would otherwise poison the mutex and turn every
/// later metadata call into a panic too.
fn lock_pool() -> MutexGuard<'static, Pool> {
    pool()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Get a usable visitor-data id, fetching a fresh one if none are available.
pub async fn visitor_data(http: &reqwest::Client, user_agent: &str) -> Result<String> {
    if let Some(id) = reusable_id() {
        return Ok(id);
    }
    let id = fetch(http, user_agent).await?;
    lock_pool().ids.push(id.clone());
    Ok(id)
}

/// The id the pool would hand out right now, if any. Split out so the
/// selection rule is testable without a network round trip.
fn reusable_id() -> Option<String> {
    let p = lock_pool();
    p.ids.iter().find(|id| !p.banned.contains(*id)).cloned()
}

/// Drop a visitor-data id after YouTube rejects a request carrying it.
///
/// The id is removed from `ids`, not merely added to `banned`. Keeping it in
/// `ids` would make the retry in `client::post` re-select the id it just
/// banned, fail identically, and give up — so one 403 would disable search,
/// artist, album, playlist import and credits for the life of the process.
/// Evicting it means the retry's `visitor_data` call finds no candidate and
/// fetches a genuinely fresh id.
pub fn ban(id: &str) {
    let mut p = lock_pool();
    p.banned.insert(id.to_owned());
    p.ids.retain(|i| i != id);
}

async fn fetch(http: &reqwest::Client, user_agent: &str) -> Result<String> {
    let resp = http
        .get(home())
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(reqwest::header::COOKIE, CONSENT_COOKIE)
        .send()
        .await?;

    if let Some(v) = yec_from_cookie(&resp) {
        return Ok(v);
    }
    let body = resp.text().await?;
    visitor_data_from_html(&body)
        .ok_or_else(|| Error::Bootstrap("home page did not expose visitor data".into()))
}

/// The `__Secure-YEC` consent-session cookie YouTube sets on the home page.
fn yec_from_cookie(resp: &reqwest::Response) -> Option<String> {
    for cookie in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        if let Ok(s) = cookie.to_str() {
            for part in s.split(';') {
                if let Some(v) = part.trim().strip_prefix("__Secure-YEC=") {
                    return Some(v.to_owned());
                }
            }
        }
    }
    None
}

fn visitor_data_from_html(html: &str) -> Option<String> {
    let re = regex::Regex::new(r#""visitorData":"([\w\-%]+?)""#).ok()?;
    re.captures(html)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_owned())
}

/// Make `id` the only visitor the pool will hand out, and un-ban it.
///
/// Replaces the pool rather than appending: a test that needs a *specific*
/// visitor on the first request can only get that if it is the sole candidate,
/// and the pool outlives individual tests. Evicting everything else also means
/// a ban left behind by another test cannot make this one fail for the wrong
/// reason.
#[cfg(test)]
pub(crate) fn seed_pool_for_test(id: &str) {
    let mut p = lock_pool();
    p.ids.clear();
    p.ids.push(id.to_owned());
    p.banned.remove(id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visitor pool is process-global, so tests that seed it must not run
    /// concurrently with each other.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// H1's regression guard: after `ban`, the banned id must not come back.
    ///
    /// Before the fix, `ban` only inserted into `banned` and left the id in
    /// `ids`, so `client::post`'s retry re-selected the id it had just banned
    /// and failed identically — one 403 then disabled search, artist, album,
    /// playlist import and credits for the life of the process. Selection is
    /// isolated in `reusable_id`, so this needs no network at all.
    #[test]
    fn a_banned_id_is_never_handed_back() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        const SEEDED: &str = "test-identity-ban-seeded-id";
        lock_pool().ids.push(SEEDED.to_owned());

        // While usable, the seeded id is what the pool hands out.
        assert_eq!(reusable_id().as_deref(), Some(SEEDED));

        ban(SEEDED);
        assert!(
            !lock_pool().ids.iter().any(|i| i == SEEDED),
            "ban must evict the id from the pool, not just mark it"
        );
        assert!(lock_pool().banned.contains(SEEDED));
        assert_eq!(
            reusable_id(),
            None,
            "the retry must not re-select the id it just banned"
        );

        // Leave the process-global pool as we found it.
        lock_pool().banned.remove(SEEDED);
    }

    /// A banned id must not shadow a usable one behind it.
    #[test]
    fn a_banned_id_does_not_hide_the_others() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        const BAD: &str = "test-identity-ban-bad";
        const GOOD: &str = "test-identity-ban-good";
        lock_pool().ids.extend([BAD.to_owned(), GOOD.to_owned()]);
        ban(BAD);
        assert_eq!(reusable_id().as_deref(), Some(GOOD));

        lock_pool().ids.retain(|i| i != GOOD);
        lock_pool().banned.remove(BAD);
    }
}
