//! `--sign-in`: the game signs its player in to Microsoft itself, without the launcher. The first
//! time it shows a device code to enter at microsoft.com/link; then it keeps the Microsoft refresh
//! token in `horizon-sign-in.json` beside the settings (owner-only on Unix) and renews the
//! Minecraft token at every start. Tokens are never printed or logged.
use crate::auth::{parse_uuid, AuthSession};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

/// Horizon's own Azure app, the one the launcher's release builds carry.
const CLIENT_ID: &str = "331e08f8-172d-43b9-835a-3108c2fcdbf3";
const SCOPE: &str = "XboxLive.signin offline_access";
const MS: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0";
const STORE: &str = "horizon-sign-in.json";

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .user_agent(&format!("Horizon/{}", horizon_core::release::VERSION))
        .build()
}

/// A JSON body (ureq's `json` feature is off here).
trait SendJson {
    fn send_json(self, body: Value) -> Result<ureq::Response, ureq::Error>;
}
impl SendJson for ureq::Request {
    fn send_json(self, body: Value) -> Result<ureq::Response, ureq::Error> {
        self.set("Content-Type", "application/json")
            .set("Accept", "application/json")
            .send_string(&body.to_string())
    }
}

/// The status and JSON body of a request; transport errors name the service only.
fn send(r: Result<ureq::Response, ureq::Error>, service: &str) -> Result<(u16, Value), String> {
    let response = match r {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(ureq::Error::Transport(_)) => return Err(format!("{service} didn't answer")),
    };
    let status = response.status();
    let body = response.into_string().unwrap_or_default();
    Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
}

fn field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn token_form(form: &[(&str, &str)]) -> Result<(u16, Value), String> {
    send(
        agent().post(&format!("{MS}/token")).send_form(form),
        "Microsoft",
    )
}

/// The Microsoft access and refresh tokens: renewed from the stored refresh token, else a new
/// device-code sign-in.
fn microsoft(stored: Option<String>) -> Result<(String, String), String> {
    if let Some(refresh) = stored {
        let (status, v) = token_form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("scope", SCOPE),
        ])?;
        if let (200, Some(a), Some(r)) = (
            status,
            field(&v, "access_token"),
            field(&v, "refresh_token"),
        ) {
            return Ok((a, r));
        }
        eprintln!("Horizon: the saved Microsoft sign-in expired, sign in again.");
    }
    let (_, code) = send(
        agent()
            .post(&format!("{MS}/devicecode"))
            .send_form(&[("client_id", CLIENT_ID), ("scope", SCOPE)]),
        "Microsoft",
    )?;
    let device = field(&code, "device_code").ok_or("Microsoft refused the sign-in")?;
    eprintln!(
        "\nSign in: open {} and enter {}\n",
        field(&code, "verification_uri").unwrap_or_default(),
        field(&code, "user_code").unwrap_or_default()
    );
    let mut interval = code.get("interval").and_then(Value::as_u64).unwrap_or(5);
    loop {
        std::thread::sleep(Duration::from_secs(interval));
        let (status, v) = token_form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", &device),
        ])?;
        if let (200, Some(a), Some(r)) = (
            status,
            field(&v, "access_token"),
            field(&v, "refresh_token"),
        ) {
            return Ok((a, r));
        }
        match field(&v, "error").as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some(e) => return Err(format!("the sign-in ended: {e}")),
            None => return Err("Microsoft refused the sign-in".into()),
        }
    }
}

/// Xbox Live, XSTS, then the Minecraft token and profile.
fn minecraft(ms_access: &str) -> Result<AuthSession, String> {
    let (status, xbl) = send(
        agent().post("https://user.auth.xboxlive.com/user/authenticate").send_json(json!({
            "Properties": {"AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": format!("d={ms_access}")},
            "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT",
        })),
        "Xbox Live",
    )?;
    let xbl = field(&xbl, "Token")
        .filter(|_| status == 200)
        .ok_or("Xbox Live refused the sign-in")?;
    let (status, xsts) = send(
        agent()
            .post("https://xsts.auth.xboxlive.com/xsts/authorize")
            .send_json(json!({
                "Properties": {"SandboxId": "RETAIL", "UserTokens": [xbl]},
                "RelyingParty": "rp://api.minecraftservices.com/", "TokenType": "JWT",
            })),
        "Xbox Live",
    )?;
    if let Some(code) = xsts.get("XErr").and_then(Value::as_u64) {
        return Err(format!("Xbox Live refused the sign-in (XErr {code})"));
    }
    let claims = xsts
        .pointer("/DisplayClaims/xui/0")
        .cloned()
        .unwrap_or(Value::Null);
    let (Some(xsts), Some(uhs)) = (
        field(&xsts, "Token").filter(|_| status == 200),
        field(&claims, "uhs"),
    ) else {
        return Err("Xbox Live refused the sign-in".into());
    };
    let (status, mc) = send(
        agent()
            .post("https://api.minecraftservices.com/authentication/login_with_xbox")
            .send_json(json!({"identityToken": format!("XBL3.0 x={uhs};{xsts}")})),
        "the Minecraft services",
    )?;
    let token = field(&mc, "access_token")
        .filter(|_| status == 200)
        .ok_or_else(|| format!("the Minecraft services refused the sign-in ({status})"))?;
    let (status, profile) = send(
        agent()
            .get("https://api.minecraftservices.com/minecraft/profile")
            .set("Authorization", &format!("Bearer {token}"))
            .call(),
        "the Minecraft services",
    )?;
    let (Some(name), Some(uuid)) = (
        field(&profile, "name").filter(|_| status == 200),
        field(&profile, "id").as_deref().and_then(parse_uuid),
    ) else {
        return Err("this account has no Minecraft: Java Edition profile".into());
    };
    let mut session = AuthSession::new(&name, uuid, &token);
    session.xuid = field(&claims, "xid");
    Ok(session)
}

fn save(path: &Path, refresh: &str) {
    let text = json!({ "refresh_token": refresh }).to_string();
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path);
        if let Ok(mut f) = file {
            let _ = f.write_all(text.as_bytes());
        }
    }
    #[cfg(not(unix))]
    let _ = std::fs::write(path, text);
}

/// Sign in (or renew the saved sign-in) before the window opens.
pub fn sign_in() -> Result<AuthSession, String> {
    let path = Path::new(STORE);
    let stored = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| field(&v, "refresh_token"));
    let (access, refresh) = microsoft(stored)?;
    save(path, &refresh);
    minecraft(&access)
}
