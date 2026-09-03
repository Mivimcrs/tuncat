//! Real-machine doctor: read-only checks of every pipeline stage.
//! Run: cargo run -p tuncat-core --example doctor
//!
//! Does NOT modify anything: adapter enumeration, keyword matching,
//! health probe and ICS enumeration are all read-only.

use anyhow::Result;
use tuncat_core::config::Config;
use tuncat_core::{detector, ics};

fn main() -> Result<()> {
    let cfg = Config::load_or_default()?;

    println!("=== 1. Adapter enumeration ===");
    let ads = detector::list_adapters()?;
    for a in &ads {
        println!(
            "  {:<28} up={:<5} gw={:<5} ip={:<16} {}",
            a.friendly_name, a.oper_up, a.has_gateway,
            a.ipv4.as_deref().unwrap_or("-"),
            a.description,
        );
    }

    println!("\n=== 2. Keyword matching ===");
    let tun = detector::find_tun(&ads, &cfg.tun_keywords);
    let public = detector::find_public(&ads, &cfg.public_keywords);
    match &tun {
        Some(t) => println!(
            "  TUN    -> '{}' (desc: {})",
            t.friendly_name, t.description
        ),
        None => println!("  TUN    -> NOT FOUND (keywords: {:?})", cfg.tun_keywords),
    }
    match &public {
        Some(p) => println!(
            "  Public -> '{}' (desc: {}, gateway: {})",
            p.friendly_name, p.description, p.has_gateway
        ),
        None => println!("  Public -> NOT FOUND"),
    }

    println!("\n=== 3. Health probe ({} timeout {}s) ===", cfg.probe_url, cfg.probe_timeout_sec);
    match detector::probe(&cfg.probe_url, cfg.probe_timeout_sec) {
        detector::ProbeResult::Healthy(lat) => {
            println!("  HEALTHY: {} ms", lat.as_millis())
        }
        detector::ProbeResult::Unhealthy(reason) => {
            println!("  UNHEALTHY: {reason}")
        }
    }

    println!("\n=== 4. ICS COM channel (read-only enumeration) ===");
    match ics::IcsPulser::new() {
        Ok(pulser) => match pulser.list_connections() {
            Ok(conns) => {
                println!("  {} connections visible to ICS:", conns.len());
                for c in &conns {
                    println!(
                        "  {:<28} sharing={:<5} role={:?} dev={}",
                        c.name, c.sharing_enabled, c.sharing_role, c.device_name
                    );
                }
                if conns.is_empty() {
                    println!("  (none - ICS may be blocked)");
                }
            }
            Err(e) => {
                println!("  ICS enumeration FAILED: {e:#}");
                if format!("{e:#}").contains("0x80070005") {
                    println!();
                    println!("  -> Access denied: ICS enumeration requires elevation.");
                    println!("  -> Run this terminal as Administrator, or just run tuncat.exe");
                    println!("     (its manifest already requests admin via UAC).");
                }
                std::process::exit(2);
            }
        },
        Err(e) => {
            println!("  ICS manager init FAILED: {e:#}");
            std::process::exit(2);
        }
    }

    println!("\nDoctor: all read-only checks finished.");
    Ok(())
}
