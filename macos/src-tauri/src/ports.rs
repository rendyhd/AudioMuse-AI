// Port allocation and conflict detection for AudioMuse-AI services.

use std::net::TcpListener;
use tracing::{info, warn};

/// Allocated ports for all services.
#[derive(Debug, Clone)]
pub struct AllocatedPorts {
    pub flask: u16,
    pub postgres: u16,
    pub redis: u16,
}

/// Default ports matching the Docker configuration.
const DEFAULT_FLASK_PORT: u16 = 8000;
const DEFAULT_POSTGRES_PORT: u16 = 5432;
const DEFAULT_REDIS_PORT: u16 = 6379;

/// Check if a port is available by attempting to bind to it.
fn is_port_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Find the next available port starting from `preferred`.
/// Tries up to 100 ports above the preferred one.
fn find_available_port(preferred: u16, service_name: &str) -> Result<u16, String> {
    if is_port_available(preferred) {
        return Ok(preferred);
    }
    warn!(
        "{} default port {} is in use, searching for alternative",
        service_name, preferred
    );
    for offset in 1..=100 {
        let candidate = preferred + offset;
        if is_port_available(candidate) {
            info!(
                "{} will use alternative port {}",
                service_name, candidate
            );
            return Ok(candidate);
        }
    }
    Err(format!(
        "Could not find an available port for {} (tried {}-{})",
        service_name,
        preferred,
        preferred + 100
    ))
}

/// Allocate ports for Flask, PostgreSQL, and Redis.
/// Uses default ports when available, finds alternatives when occupied.
pub fn allocate_ports() -> Result<AllocatedPorts, String> {
    let flask = find_available_port(DEFAULT_FLASK_PORT, "Flask")?;
    let postgres = find_available_port(DEFAULT_POSTGRES_PORT, "PostgreSQL")?;
    let redis = find_available_port(DEFAULT_REDIS_PORT, "Redis")?;

    Ok(AllocatedPorts {
        flask,
        postgres,
        redis,
    })
}
