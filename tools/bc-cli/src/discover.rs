//! `bc-cli discover`: search the local network for NVRs and cameras and
//! print what answered as a table. Read-only broadcasts; no login, no TCP.

use std::net::Ipv4Addr;
use std::process::ExitCode;
use std::time::Duration;

use discovery::{DiscoveredDevice, Options, ServiceKind};

pub const USAGE: &str = "\
  bc-cli discover [--interface ADDR]... [--seconds S] [--rounds N]
         [--no-native] [--no-probe] [--no-onvif]

discover broadcasts the native search (UDP 2000, answers on UDP 3000), the
native probe (UDP 2015) and ONVIF WS-Discovery on every IPv4 interface that
is up, or on each --interface (an address of this computer), listens for
--seconds (default 3), and prints every device that answered, IDs and MAC
addresses included: keep the output out of the repository. Nothing but
discovery broadcasts is sent; no device is logged in to.";

struct DiscoverArgs {
    options: Options,
}

fn parse(args: &[String]) -> Result<DiscoverArgs, String> {
    let mut options = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--interface" => {
                let v = value("--interface")?;
                let addr: Ipv4Addr = v
                    .parse()
                    .map_err(|_| format!("--interface needs an IPv4 address, not {v}"))?;
                options.interfaces.push(addr);
            }
            "--seconds" => {
                options.duration = value("--seconds")?
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && (0.1..=60.0).contains(s))
                    .map(Duration::from_secs_f64)
                    .ok_or("bad --seconds (0.1 to 60)")?;
            }
            "--rounds" => {
                options.rounds = value("--rounds")?
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=10).contains(n))
                    .ok_or("bad --rounds (1 to 10)")?;
            }
            "--no-native" => options.native_search = false,
            "--no-probe" => options.native_probe = false,
            "--no-onvif" => options.onvif = false,
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    Ok(DiscoverArgs { options })
}

/// Runs `bc-cli discover` with the arguments after `discover`.
pub fn main(args: &[String]) -> ExitCode {
    let parsed = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bc-cli: {e}\n\nusage:\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bc-cli: cannot start the runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    if parsed.options.interfaces.is_empty() {
        match discovery::interfaces() {
            Ok(list) => {
                let names: Vec<String> = list
                    .iter()
                    .map(|i| format!("{} ({})", i.name, i.address))
                    .collect();
                eprintln!("searching on {}", names.join(", "));
            }
            Err(e) => eprintln!("bc-cli: cannot list interfaces: {e}"),
        }
    }
    let report = match runtime.block_on(discovery::discover(&parsed.options)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("bc-cli: discovery failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    for e in &report.errors {
        eprintln!("warning: {e}");
    }
    print!("{}", table(&report.devices));
    eprintln!(
        "{} device(s); {} other datagram(s) ignored",
        report.devices.len(),
        report.ignored
    );
    ExitCode::SUCCESS
}

/// The devices as an aligned text table.
fn table(devices: &[DiscoveredDevice]) -> String {
    let header = [
        "KIND", "NAME", "MODEL", "ADDRESS", "PORT", "CH", "BATTERY", "MAC", "ONVIF", "ID",
    ];
    let dash = || "-".to_owned();
    let rows: Vec<Vec<String>> = devices
        .iter()
        .map(|d| {
            vec![
                d.kind.as_str().to_owned(),
                d.name.clone().unwrap_or_else(dash),
                d.model.clone().unwrap_or_else(dash),
                d.address.to_string(),
                d.native_port().map_or_else(dash, |p| p.to_string()),
                d.channels.map_or_else(dash, |c| c.to_string()),
                d.battery_percent.map_or_else(dash, |b| format!("{b}%")),
                d.mac.clone().unwrap_or_else(dash),
                d.service(ServiceKind::Onvif)
                    .and_then(|s| s.url.clone())
                    .unwrap_or_else(dash),
                d.device_id.clone().unwrap_or_else(dash),
            ]
        })
        .collect();
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    let mut line = |cells: Vec<String>| {
        let parts: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect();
        out.push_str(parts.join("  ").trim_end());
        out.push('\n');
    };
    line(header.iter().map(|h| (*h).to_owned()).collect());
    for row in rows {
        line(row);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use discovery::{DeviceKind, Service};
    use std::net::IpAddr;

    #[test]
    fn parses_options() {
        let a: Vec<String> = ["--interface", "192.0.2.5", "--seconds", "1.5", "--no-onvif"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let p = parse(&a).unwrap();
        assert_eq!(p.options.interfaces, vec![Ipv4Addr::new(192, 0, 2, 5)]);
        assert_eq!(p.options.duration, Duration::from_millis(1500));
        assert!(!p.options.onvif && p.options.native_search);
        assert!(parse(&["--interface".into(), "nope".into()]).is_err());
        assert!(parse(&["--seconds".into(), "0".into()]).is_err());
        assert!(parse(&["--bogus".into()]).is_err());
    }

    #[test]
    fn table_aligns_and_fills_gaps() {
        let mut d = DiscoveredDevice::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
        d.kind = DeviceKind::Nvr;
        d.name = Some("NVR 1".into());
        d.channels = Some(16);
        d.services.push(Service {
            kind: ServiceKind::Native,
            port: Some(9000),
            url: None,
        });
        let t = table(&[d]);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("KIND  NAME   MODEL  ADDRESS"));
        assert!(lines[1].starts_with("nvr   NVR 1  -      192.0.2.10  9000  16"));
    }
}
