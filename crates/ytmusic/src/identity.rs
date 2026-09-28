//! Anonymous visitor-data identity and rotation.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use crate::context::HOME;
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

/// Get a usable visitor-data id, fetching a fresh one if none are available.
pub async fn visitor_data(http: &reqwest::Client, user_agent: &str) -> Result<String> {
    {
        let p = pool().lock().unwrap();
        if let Some(id) = p.ids.iter().find(|id| !p.banned.contains(*id)) {
            return Ok(id.clone());
        }
    }
    let id = fetch(http, user_agent).await?;
    pool().lock().unwrap().ids.push(id.clone());
    Ok(id)
}

/// Drop a visitor-data id after YouTube rejects a request carrying it.
pub fn ban(id: &str) {
    pool().lock().unwrap().banned.insert(id.to_owned());
}

async fn fetch(http: &reqwest::Client, user_agent: &str) -> Result<String> {
    let resp = http
        .get(HOME)
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
