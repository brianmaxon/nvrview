//! sign-update: the update publisher's tool (docs/UPDATES.md).

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage:
  sign-update keygen --secret FILE [--public FILE]
  sign-update pubkey --secret FILE
  sign-update manifest --version VERSION [--notes TEXT | --notes-file FILE]
              [--expires-in DAYS] --out DIR/updates.json PACKAGE.zip...
  sign-update sign --secret FILE [--append] DIR/updates.json
  sign-update check --source FOLDER|URL --key FILE|KEY [--key ...]
              --current VERSION [--platform NAME] [--stage DIR]

keygen writes a new secret key (mode 0600, never over an existing file, never
inside a git working tree) and prints the public key line to give to users.
manifest reads each package's platform from its name,
<name>-<version>-<platform>.zip, and records its size and SHA-256; it dates
the manifest now (published) and, with --expires-in, sets when apps stop
accepting it (expires).
sign writes updates.json.sig next to the manifest; --append adds a line for a
second key (key rotation).
check does what the apps do: fetch and verify the manifest, compare versions,
and, when a newer version is offered, download, verify, and unpack it (into
--stage DIR, kept, or a temporary folder, removed).
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sign-update: {e}");
            if e.starts_with("usage") {
                eprint!("{USAGE}");
            }
            ExitCode::FAILURE
        }
    }
}

struct Args {
    flags: Vec<(String, Option<String>)>,
    positional: Vec<String>,
}

impl Args {
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> Result<Args, String> {
        let mut flags = Vec::new();
        let mut positional = Vec::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if with_value.contains(&a.as_str()) {
                let v = it
                    .next()
                    .ok_or_else(|| format!("usage: {a} needs a value"))?;
                flags.push((a.clone(), Some(v.clone())));
            } else if switches.contains(&a.as_str()) {
                flags.push((a.clone(), None));
            } else if a.starts_with("--") {
                return Err(format!("usage: unknown option {a}"));
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Args { flags, positional })
    }

    fn one(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.flags
            .iter()
            .filter(|(n, _)| n == name)
            .filter_map(|(_, v)| v.as_deref())
            .collect()
    }

    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }

    fn need(&self, name: &str) -> Result<&str, String> {
        self.one(name)
            .ok_or_else(|| format!("usage: {name} is required"))
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let (command, rest) = args.split_first().ok_or("usage: no command")?;
    match command.as_str() {
        "keygen" => {
            let a = Args::parse(rest, &["--secret", "--public"], &[])?;
            let public = a.one("--public").map(PathBuf::from);
            let key = sign_update::keygen(&PathBuf::from(a.need("--secret")?), public.as_deref())?;
            println!("{}", key.to_text());
            println!("key id: {}", key.id());
            Ok(())
        }
        "pubkey" => {
            let a = Args::parse(rest, &["--secret"], &[])?;
            let key = sign_update::read_secret(&PathBuf::from(a.need("--secret")?))?.public();
            println!("{}", key.to_text());
            println!("key id: {}", key.id());
            Ok(())
        }
        "manifest" => {
            let a = Args::parse(
                rest,
                &[
                    "--version",
                    "--notes",
                    "--notes-file",
                    "--out",
                    "--expires-in",
                ],
                &[],
            )?;
            let notes = match (a.one("--notes"), a.one("--notes-file")) {
                (Some(_), Some(_)) => return Err("usage: --notes or --notes-file, not both".into()),
                (Some(n), None) => n.to_string(),
                (None, Some(f)) => {
                    std::fs::read_to_string(f).map_err(|e| format!("reading {f}: {e}"))?
                }
                (None, None) => String::new(),
            };
            let packages: Vec<PathBuf> = a.positional.iter().map(PathBuf::from).collect();
            let now = updates::time::now();
            let expires = match a.one("--expires-in") {
                None => None,
                Some(d) => {
                    let days: u64 = d
                        .parse()
                        .ok()
                        .filter(|n| (1..=3650).contains(n))
                        .ok_or("--expires-in takes a number of days, 1 to 3650")?;
                    Some(now + days * 86_400)
                }
            };
            let text =
                sign_update::build_manifest(a.need("--version")?, &notes, &packages, now, expires)?;
            let out = PathBuf::from(a.need("--out")?);
            std::fs::write(&out, text).map_err(|e| format!("writing {}: {e}", out.display()))?;
            println!("wrote {}", out.display());
            Ok(())
        }
        "sign" => {
            let a = Args::parse(rest, &["--secret"], &["--append"])?;
            let [manifest] = a.positional.as_slice() else {
                return Err("usage: sign takes one manifest".into());
            };
            let key = sign_update::read_secret(&PathBuf::from(a.need("--secret")?))?;
            let sig = sign_update::sign(&PathBuf::from(manifest), &key, a.has("--append"))?;
            println!("wrote {} (key {})", sig.display(), key.public().id());
            Ok(())
        }
        "check" => {
            let a = Args::parse(
                rest,
                &["--source", "--key", "--current", "--platform", "--stage"],
                &[],
            )?;
            if !a.positional.is_empty() {
                return Err("usage: check takes no positional arguments".into());
            }
            let keys = a
                .all("--key")
                .into_iter()
                .map(sign_update::read_public)
                .collect::<Result<Vec<_>, _>>()?;
            let stage = a.one("--stage").map(PathBuf::from);
            let report = sign_update::check(
                a.need("--source")?,
                keys,
                a.need("--current")?,
                a.one("--platform"),
                stage.as_deref(),
                updates::time::now(),
            )?;
            for line in report.lines {
                println!("{line}");
            }
            Ok(())
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(format!("usage: unknown command {other}")),
    }
}
