#![allow(dead_code)]
//! Shared helpers for the Phase 3 durable-state tests.
use invoiceshield_governor::Master;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

pub const CLEAN: &str = r#"{"signals":[]}"#;

pub fn master_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../dataset/fixtures/master.json")
}

pub fn master() -> Master {
    Master::from_json(&std::fs::read_to_string(master_path()).unwrap()).unwrap()
}

/// A V001 invoice against PO-2026-0101 that the unchanged rules ALLOW.
pub fn body(invoice_no: &str, rupees: i64) -> String {
    serde_json::json!({
        "proposal": {"vendor_id": "V001", "invoice_no": invoice_no, "amount_paise": rupees * 100, "po_ref": "PO-2026-0101"},
        "evidence": {"text_layer_gstin": "24ZZACS4821A1ZX", "sender_domain": "sharmaofficesupplies.in", "has_text_layer": true, "text_source": "text_layer"},
        "checker_raw": CLEAN
    })
    .to_string()
}

static N: AtomicU32 = AtomicU32::new(0);

/// Fresh empty directory under the system temp dir.
pub fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "isg-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}
