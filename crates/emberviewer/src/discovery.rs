//! mDNS / DNS-SD discovery of Ember+ providers (`_ember._tcp`).

use std::collections::HashMap;

use mdns_sd::{Receiver, ServiceDaemon, ServiceEvent};

/// The DNS-SD service type Ember+ providers advertise.
pub const SERVICE_TYPE: &str = "_ember._tcp.local.";

/// A discovered provider.
#[derive(Clone, Debug)]
pub struct Discovered {
    pub instance: String,
    pub host: String,
    pub port: u16,
}

impl Discovered {
    /// A friendly display name (the instance label without the service suffix).
    pub fn display_name(&self) -> String {
        self.instance
            .split('.')
            .next()
            .unwrap_or(&self.instance)
            .to_string()
    }
}

/// A running mDNS browse. Dropping it stops discovery.
pub struct Discovery {
    daemon: ServiceDaemon,
    receiver: Receiver<ServiceEvent>,
    /// Resolved providers, keyed by fullname.
    pub found: HashMap<String, Discovered>,
}

impl Discovery {
    pub fn start() -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
        let receiver = daemon.browse(SERVICE_TYPE).map_err(|e| e.to_string())?;
        Ok(Discovery {
            daemon,
            receiver,
            found: HashMap::new(),
        })
    }

    /// Drain pending events. Returns true if the discovered set changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.receiver.try_recv() {
            match event {
                ServiceEvent::ServiceResolved(info) => {
                    // Prefer a resolved IP address; fall back to the hostname.
                    let host = info
                        .get_addresses()
                        .iter()
                        .next()
                        .map(|a| a.to_string())
                        .unwrap_or_else(|| info.get_hostname().trim_end_matches('.').to_string());
                    let d = Discovered {
                        instance: info.get_fullname().to_string(),
                        host,
                        port: info.get_port(),
                    };
                    self.found.insert(d.instance.clone(), d);
                    changed = true;
                }
                ServiceEvent::ServiceRemoved(_ty, fullname) => {
                    changed |= self.found.remove(&fullname).is_some();
                }
                _ => {}
            }
        }
        changed
    }

    /// Discovered providers, sorted by display name.
    pub fn sorted(&self) -> Vec<Discovered> {
        let mut v: Vec<Discovered> = self.found.values().cloned().collect();
        v.sort_by_key(|a| a.display_name());
        v
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}
