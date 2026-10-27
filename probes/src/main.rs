//! Feasibility probes (brief §17).
//!
//! Each probe logs in to one service through its official API, prints the
//! latest messages of a few conversations and opens the realtime socket once.
//! Nothing is written to disk. Values can be typed interactively or passed via
//! environment variables (see README).

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::Write;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::HeaderValue, Message as WsMessage,
};

#[tokio::main]
async fn main() -> Result<()> {
    let which = std::env::args().nth(1).unwrap_or_default();
    let result = match which.as_str() {
        "slack" => slack().await,
        "mattermost" | "mm" => mattermost().await,
        "rocketchat" | "rc" => rocketchat().await,
        _ => {
            eprintln!("usage: agora-probes <slack|mattermost|rocketchat>");
            return Ok(());
        }
    };
    if let Err(e) = &result {
        eprintln!("\nFAILED: {e:#}");
    }
    result
}

// ---------------------------------------------------------------- helpers

fn ask(label: &str, env: &str) -> Result<String> {
    if let Ok(v) = std::env::var(env) {
        if !v.trim().is_empty() {
            return Ok(v.trim().to_string());
        }
    }
    print!("{label}: ");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

fn ask_secret(label: &str, env: &str) -> Result<String> {
    if let Ok(v) = std::env::var(env) {
        if !v.trim().is_empty() {
            return Ok(v.trim().to_string());
        }
    }
    Ok(rpassword::prompt_password(format!("{label}: "))?.trim().to_string())
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("Agora-probe/0.1")
        .timeout(Duration::from_secs(30))
        .build()?)
}

fn server_url(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    if s.starts_with("http://") || s.starts_with("https://") {
        s.to_string()
    } else {
        format!("https://{s}")
    }
}

fn ws_url(server: &str, path: &str) -> String {
    let base = server
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{base}{path}")
}

fn one_line(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 100 {
        format!("{}…", flat.chars().take(100).collect::<String>())
    } else {
        flat
    }
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Reads socket frames for up to `secs` seconds, printing the first few, and
/// returns whether a frame satisfying `ok` was seen.
async fn watch_socket<S>(ws: &mut S, secs: u64, ok: impl Fn(&Value) -> bool) -> bool
where
    S: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut shown = 0;
    while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, ws.next()).await {
        let Ok(WsMessage::Text(text)) = frame else { continue };
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if shown < 3 {
            println!("    <- {}", one_line(&text));
            shown += 1;
        }
        if ok(&v) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------- Slack

async fn slack_call(
    c: &reqwest::Client,
    base: &str,
    token: &str,
    cookie: &str,
    method: &str,
    params: &[(&str, &str)],
) -> Result<Value> {
    let mut form: Vec<(&str, &str)> = vec![("token", token)];
    form.extend_from_slice(params);
    let resp = c
        .post(format!("{base}/api/{method}"))
        .header("Cookie", format!("d={cookie}"))
        .form(&form)
        .send()
        .await
        .with_context(|| format!("{method}: request failed"))?;
    if resp.status().as_u16() == 429 {
        let wait = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("?")
            .to_string();
        bail!("{method}: rate limited, retry after {wait}s");
    }
    let v: Value = resp.json().await.with_context(|| format!("{method}: bad JSON"))?;
    if v["ok"].as_bool() != Some(true) {
        bail!("{method}: {}", v["error"].as_str().unwrap_or("unknown error"));
    }
    Ok(v)
}

async fn slack() -> Result<()> {
    println!("Slack probe: needs the web-client token (xoxc-...) and the `d` cookie (xoxd-...).");
    let token = ask_secret("xoxc token", "SLACK_TOKEN")?;
    let raw_cookie = ask_secret("d cookie", "SLACK_D_COOKIE")?;
    let base = std::env::var("SLACK_WORKSPACE_URL")
        .map(|s| server_url(&s))
        .unwrap_or_else(|_| "https://slack.com".into());
    // Browsers show the cookie already URL-encoded; encode it only if it is not.
    let cookie = if raw_cookie.contains('%') { raw_cookie } else { pct_encode(&raw_cookie) };
    let c = http()?;

    let auth = slack_call(&c, &base, &token, &cookie, "auth.test", &[]).await?;
    println!(
        "OK  authenticated as {} on {} ({})",
        auth["user"].as_str().unwrap_or("?"),
        auth["team"].as_str().unwrap_or("?"),
        auth["url"].as_str().unwrap_or("?")
    );

    let list = slack_call(
        &c,
        &base,
        &token,
        &cookie,
        "conversations.list",
        &[
            ("types", "public_channel,private_channel,mpim,im"),
            ("exclude_archived", "true"),
            ("limit", "200"),
        ],
    )
    .await?;
    let chans = list["channels"].as_array().cloned().unwrap_or_default();
    println!("OK  {} conversations visible (first page)", chans.len());

    match slack_call(&c, &base, &token, &cookie, "client.counts", &[]).await {
        Ok(v) => {
            let unread = ["channels", "mpims", "ims"]
                .iter()
                .flat_map(|k| v[*k].as_array().cloned().unwrap_or_default())
                .filter(|x| x["has_unreads"].as_bool() == Some(true))
                .count();
            println!("OK  client.counts works: {unread} conversations with unread messages");
        }
        Err(e) => println!("--  client.counts unavailable ({e}); catch-up would fall back to per-channel checks"),
    }

    let picked: Vec<&Value> = chans
        .iter()
        .filter(|ch| ch["is_member"].as_bool() == Some(true) || ch["is_im"].as_bool() == Some(true))
        .take(3)
        .collect();
    for ch in picked {
        let id = ch["id"].as_str().unwrap_or_default();
        let name = ch["name"].as_str().or(ch["user"].as_str()).unwrap_or(id);
        println!("\n  #{name}");
        match slack_call(&c, &base, &token, &cookie, "conversations.history", &[("channel", id), ("limit", "5")]).await {
            Ok(h) => {
                let msgs = h["messages"].as_array().cloned().unwrap_or_default();
                for m in msgs.iter().rev() {
                    println!(
                        "    [{}] {}: {}",
                        m["ts"].as_str().unwrap_or(""),
                        m["user"].as_str().unwrap_or("bot"),
                        one_line(m["text"].as_str().unwrap_or(""))
                    );
                }
            }
            Err(e) => println!("    history failed: {e}"),
        }
    }

    println!("\nRealtime: rtm.connect");
    let rtm = slack_call(&c, &base, &token, &cookie, "rtm.connect", &[]).await?;
    let url = rtm["url"].as_str().ok_or_else(|| anyhow!("rtm.connect returned no url"))?;
    let mut req = url.into_client_request()?;
    req.headers_mut()
        .insert("Cookie", HeaderValue::from_str(&format!("d={cookie}"))?);
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.context("websocket connect")?;
    if watch_socket(&mut ws, 15, |v| v["type"] == "hello").await {
        println!("OK  realtime socket open (hello received)");
    } else {
        println!("--  socket opened but no hello within 15 s");
    }
    let _ = ws.close(None).await;
    println!("\nSlack probe finished.");
    Ok(())
}

// ---------------------------------------------------------------- Mattermost

async fn mattermost() -> Result<()> {
    println!("Mattermost probe.");
    let server = server_url(&ask("Server URL (e.g. https://chat.example.org)", "MM_SERVER")?);
    let c = http()?;

    let token = match std::env::var("MM_TOKEN") {
        Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => {
            let login = ask("Username or email (empty = paste a token instead)", "MM_LOGIN")?;
            if login.is_empty() {
                ask_secret("Personal access token or MMAUTHTOKEN cookie", "MM_TOKEN")?
            } else {
                let password = ask_secret("Password", "MM_PASSWORD")?;
                let mut body = json!({ "login_id": login, "password": password });
                let mut resp = c.post(format!("{server}/api/v4/users/login")).json(&body).send().await?;
                if !resp.status().is_success() {
                    let err: Value = resp.json().await.unwrap_or_default();
                    let id = err["id"].as_str().unwrap_or_default();
                    if !id.contains("mfa") {
                        bail!(
                            "login failed: {} ({id}). If your server uses SSO, rerun and paste a token.",
                            err["message"].as_str().unwrap_or("?")
                        );
                    }
                    body["token"] = json!(ask("MFA code", "MM_MFA")?);
                    resp = c.post(format!("{server}/api/v4/users/login")).json(&body).send().await?;
                    if !resp.status().is_success() {
                        let err: Value = resp.json().await.unwrap_or_default();
                        bail!("login failed: {}", err["message"].as_str().unwrap_or("?"));
                    }
                }
                resp.headers()
                    .get("Token")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("login succeeded but no Token header"))?
            }
        }
    };

    let get = |path: String| {
        let c = c.clone();
        let token = token.clone();
        let server = server.clone();
        async move {
            let r = c.get(format!("{server}/api/v4{path}")).bearer_auth(&token).send().await?;
            if !r.status().is_success() {
                bail!("GET {path}: HTTP {}", r.status());
            }
            Ok::<Value, anyhow::Error>(r.json().await?)
        }
    };

    let me = get("/users/me".into()).await?;
    println!("OK  authenticated as @{}", me["username"].as_str().unwrap_or("?"));
    let teams = get("/users/me/teams".into()).await?;
    let teams = teams.as_array().cloned().unwrap_or_default();
    println!(
        "OK  teams: {}",
        teams.iter().filter_map(|t| t["display_name"].as_str()).collect::<Vec<_>>().join(", ")
    );
    let team = teams.first().ok_or_else(|| anyhow!("no teams"))?;
    let team_id = team["id"].as_str().unwrap_or_default();

    let chans = get(format!("/users/me/teams/{team_id}/channels")).await?;
    let mut chans = chans.as_array().cloned().unwrap_or_default();
    chans.sort_by_key(|ch| std::cmp::Reverse(ch["last_post_at"].as_i64().unwrap_or(0)));
    println!("OK  {} channels in first team", chans.len());

    for ch in chans.iter().take(3) {
        let id = ch["id"].as_str().unwrap_or_default();
        let name = ch["display_name"].as_str().filter(|s| !s.is_empty()).or(ch["name"].as_str()).unwrap_or(id);
        println!("\n  #{name}");
        let posts = get(format!("/channels/{id}/posts?per_page=5")).await?;
        let order = posts["order"].as_array().cloned().unwrap_or_default();
        for pid in order.iter().rev() {
            let p = &posts["posts"][pid.as_str().unwrap_or_default()];
            println!(
                "    [{}] {}: {}",
                p["create_at"].as_i64().unwrap_or(0),
                p["user_id"].as_str().unwrap_or("?"),
                one_line(p["message"].as_str().unwrap_or(""))
            );
        }
    }

    println!("\nRealtime: /api/v4/websocket");
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&server, "/api/v4/websocket"))
        .await
        .context("websocket connect")?;
    let challenge = json!({ "seq": 1, "action": "authentication_challenge", "data": { "token": token } });
    ws.send(WsMessage::Text(challenge.to_string())).await?;
    if watch_socket(&mut ws, 10, |v| v["seq_reply"] == 1 && v["status"] == "OK").await {
        println!("OK  realtime socket authenticated");
    } else {
        println!("--  no authentication reply within 10 s");
    }
    let _ = ws.close(None).await;
    println!("\nMattermost probe finished.");
    Ok(())
}

// ---------------------------------------------------------------- Rocket.Chat

async fn rocketchat() -> Result<()> {
    println!("Rocket.Chat probe.");
    let server = server_url(&ask("Server URL (e.g. https://chat.example.org)", "RC_SERVER")?);
    let c = http()?;

    let (user_id, token) = match (std::env::var("RC_USER_ID"), std::env::var("RC_TOKEN")) {
        (Ok(u), Ok(t)) if !u.is_empty() && !t.is_empty() => (u, t),
        _ => {
            let user = ask("Username or email", "RC_LOGIN")?;
            let password = ask_secret("Password", "RC_PASSWORD")?;
            let mut body = json!({ "user": user, "password": password });
            let mut v: Value = c.post(format!("{server}/api/v1/login")).json(&body).send().await?.json().await?;
            if v["status"] != "success" {
                let e = v["error"].as_str().unwrap_or_default().to_string();
                if !e.contains("totp") && !e.contains("2fa") {
                    bail!("login failed: {}", v["message"].as_str().unwrap_or(&e));
                }
                body["code"] = json!(ask("2FA code", "RC_2FA")?);
                v = c.post(format!("{server}/api/v1/login")).json(&body).send().await?.json().await?;
                if v["status"] != "success" {
                    bail!("login failed: {}", v["message"].as_str().unwrap_or("?"));
                }
            }
            (
                v["data"]["userId"].as_str().unwrap_or_default().to_string(),
                v["data"]["authToken"].as_str().unwrap_or_default().to_string(),
            )
        }
    };

    let get = |path: String| {
        let c = c.clone();
        let (server, user_id, token) = (server.clone(), user_id.clone(), token.clone());
        async move {
            let r = c
                .get(format!("{server}/api/v1/{path}"))
                .header("X-User-Id", user_id)
                .header("X-Auth-Token", token)
                .send()
                .await?;
            if !r.status().is_success() {
                bail!("GET {path}: HTTP {}", r.status());
            }
            Ok::<Value, anyhow::Error>(r.json().await?)
        }
    };

    let me = get("me".into()).await?;
    println!("OK  authenticated as @{}", me["username"].as_str().unwrap_or("?"));
    let rooms = get("rooms.get".into()).await?;
    let mut rooms = rooms["update"].as_array().cloned().unwrap_or_default();
    rooms.sort_by_key(|r| std::cmp::Reverse(r["lm"].as_str().unwrap_or("").to_string()));
    println!("OK  {} rooms", rooms.len());

    for r in rooms.iter().take(3) {
        let id = r["_id"].as_str().unwrap_or_default();
        let kind = r["t"].as_str().unwrap_or("c");
        let name = r["fname"].as_str().or(r["name"].as_str()).unwrap_or(id);
        let endpoint = match kind {
            "p" => "groups.history",
            "d" => "im.history",
            _ => "channels.history",
        };
        println!("\n  #{name} ({kind})");
        match get(format!("{endpoint}?roomId={id}&count=5")).await {
            Ok(h) => {
                let msgs = h["messages"].as_array().cloned().unwrap_or_default();
                for m in msgs.iter().rev() {
                    println!(
                        "    [{}] {}: {}",
                        m["ts"].as_str().unwrap_or(""),
                        m["u"]["username"].as_str().unwrap_or("?"),
                        one_line(m["msg"].as_str().unwrap_or(""))
                    );
                }
            }
            Err(e) => println!("    history failed: {e}"),
        }
    }

    println!("\nRealtime: DDP /websocket");
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url(&server, "/websocket"))
        .await
        .context("websocket connect")?;
    ws.send(WsMessage::Text(json!({ "msg": "connect", "version": "1", "support": ["1"] }).to_string()))
        .await?;
    ws.send(WsMessage::Text(
        json!({ "msg": "method", "method": "login", "id": "1", "params": [{ "resume": token }] }).to_string(),
    ))
    .await?;
    if watch_socket(&mut ws, 10, |v| v["msg"] == "result" && v["id"] == "1" && v.get("error").is_none()).await {
        println!("OK  realtime socket authenticated");
    } else {
        println!("--  no login result within 10 s");
    }
    let _ = ws.close(None).await;
    println!("\nRocket.Chat probe finished.");
    Ok(())
}
