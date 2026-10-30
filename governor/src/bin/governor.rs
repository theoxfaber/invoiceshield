//! Operator CLI for the governor's durable state.
//!
//!   governor backup  --db governor.db --out backup.db       online backup (SQLite backup API)
//!   governor restore --from backup.db --db governor.db [--force]
//!   governor verify  --db governor.db [--anchors F --pubkey HEX]   chain + anchors
//!   governor anchor  --db governor.db --anchors F            sign the current head now
//!                    (key from GOVERNOR_ANCHOR_KEY / GOVERNOR_ANCHOR_KEY_FILE)
//!   governor keygen                                          new Ed25519 seed + pubkey
//!   governor new-key --name NAME --role agent|operator|approver[,..]
//!                    new random API key (printed once) + its GOVERNOR_IDENTITIES entry (hash only)
//!
//! Exit codes: 0 ok, 1 verification failed, 2 usage or I/O error.
use invoiceshield_governor::{anchor, persist};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn usage() -> ! {
    eprintln!("usage: governor <backup|restore|verify|anchor|keygen|new-key> [--name N --role R] [--db PATH] [--out PATH] [--from PATH] [--anchors PATH] [--pubkey HEX] [--force]");
    std::process::exit(2)
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn need(args: &[String], name: &str) -> String {
    arg(args, name).unwrap_or_else(|| {
        eprintln!("missing {name}");
        usage()
    })
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else { usage() };
    match cmd.as_str() {
        "backup" => {
            let db = need(&args, "--db");
            let out = need(&args, "--out");
            if Path::new(&out).exists() {
                die(format!("{out} exists; refusing to overwrite"));
            }
            let conn = persist::connect(&db).unwrap_or_else(|e| die(e));
            persist::backup(&conn, Path::new(&out)).unwrap_or_else(|e| die(e));
            let n = persist::audit_head(&conn).unwrap_or_else(|e| die(e)).map(|h| h.2).unwrap_or(0);
            println!("backup ok: {db} -> {out} ({n} audit records)");
        }
        "restore" => {
            let from = need(&args, "--from");
            let db = need(&args, "--db");
            if Path::new(&db).exists() && !args.iter().any(|a| a == "--force") {
                die(format!("{db} exists; stop the server and pass --force to overwrite it"));
            }
            persist::restore(Path::new(&from), &db).unwrap_or_else(|e| die(e));
            println!("restore ok: {from} -> {db}");
        }
        "verify" => {
            let db = need(&args, "--db");
            let conn = persist::connect(&db).unwrap_or_else(|e| die(e));
            let records = persist::load_audit(&conn).unwrap_or_else(|e| die(e));
            let report = match (arg(&args, "--anchors"), arg(&args, "--pubkey").or_else(|| std::env::var("GOVERNOR_ANCHOR_PUBKEY").ok())) {
                (Some(f), Some(pk)) => {
                    let pk = anchor::pubkey_from_hex(&pk).unwrap_or_else(|e| die(e));
                    let list = anchor::read_all(Path::new(&f)).unwrap_or_else(|e| die(e));
                    anchor::verify_chain(&records, &list, &pk)
                }
                (Some(_), None) => die("--anchors needs --pubkey (or GOVERNOR_ANCHOR_PUBKEY): anchors are only trusted under a pinned key"),
                _ => {
                    eprintln!("warning: no anchors given; a whole-chain rewrite cannot be detected without them");
                    anchor::verify_chain(&records, &[], &anchor::generate_key().verifying_key())
                }
            };
            println!("{}", serde_json::to_string_pretty(&report).expect("json"));
            std::process::exit(if report.ok { 0 } else { 1 });
        }
        "anchor" => {
            let db = need(&args, "--db");
            let f = need(&args, "--anchors");
            let key = anchor::key_from_env().unwrap_or_else(|e| die(e)).unwrap_or_else(|| die("set GOVERNOR_ANCHOR_KEY or GOVERNOR_ANCHOR_KEY_FILE"));
            let conn = persist::connect(&db).unwrap_or_else(|e| die(e));
            let Some((seq, hash, _)) = persist::audit_head(&conn).unwrap_or_else(|e| die(e)) else { die("audit log is empty; nothing to anchor") };
            let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
            let a = anchor::sign(&key, seq, &hash, ts);
            anchor::append(Path::new(&f), &a).unwrap_or_else(|e| die(e));
            println!("{}", serde_json::to_string(&a).expect("json"));
        }
        "keygen" => {
            let k = anchor::generate_key();
            println!("GOVERNOR_ANCHOR_KEY={}", hex::encode(k.to_bytes()));
            println!("GOVERNOR_ANCHOR_PUBKEY={}", hex::encode(k.verifying_key().to_bytes()));
            eprintln!("store the key in a secrets manager or a 0600 file (GOVERNOR_ANCHOR_KEY_FILE); never commit it");
        }
        "new-key" => {
            let name = need(&args, "--name");
            let roles: Vec<String> = need(&args, "--role").split(',').map(|r| r.trim().to_lowercase()).collect();
            let entry = serde_json::json!({"name": name, "roles": roles, "key_sha256": "0".repeat(64)});
            // Validate name/roles exactly as the server will.
            let check = serde_json::json!([entry]).to_string();
            if let Err(e) = invoiceshield_governor::auth::KeyRing::from_lookup(&|k| match k {
                "GOVERNOR_IDENTITIES" => Some(check.clone()),
                "GOVERNOR_API_KEY" if !roles.iter().any(|r| r == "agent") => Some("placeholder-agent-key-for-validation".into()),
                _ => None,
            }) {
                die(e.join("; "));
            }
            let key = invoiceshield_governor::auth::new_key();
            let entry = serde_json::json!({"name": name, "roles": roles, "key_sha256": invoiceshield_governor::auth::hash_key_hex(&key)});
            println!("key={key}");
            println!("identity={entry}");
            eprintln!("give the key to {name} only (it is not stored anywhere); add the identity line to GOVERNOR_IDENTITIES_FILE");
        }
        _ => usage(),
    }
}
