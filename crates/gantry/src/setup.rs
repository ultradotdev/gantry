//! `gantry setup`: the first run, asked as questions.
//!
//! It finds the UniFi Protect console, shows the certificate it presents
//! for the owner to confirm, signs in once to prove the account works,
//! writes `config.toml`, offers to keep the password in the Keychain and
//! installs the service. Every step can be done by hand instead; this
//! only saves editing files. Running it again changes nothing that is
//! already in place.

use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use gantry_protect::{fetch_fingerprint, Client, Error, Fingerprint};

use crate::keychain::{self, Failure};
use crate::password::Secret;
use crate::protect_bridge::LOCAL_NETWORK_BLOCKED;
use crate::say::{self, bold, done};
use crate::{client, config, daemon, service, tty, video};

/// How the questions are asked, so that tests can answer them.
pub trait Prompter {
    /// One line of text. An empty answer takes the default, if there is one.
    fn line(&mut self, prompt: &str, default: Option<&str>) -> Result<String>;
    /// Yes or no. An empty answer takes the default.
    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool>;
    /// A password, typed unseen.
    fn secret(&mut self, prompt: &str) -> Result<Secret>;
    /// Waits for Return. False when they would rather skip what follows.
    fn proceed(&mut self, prompt: &str) -> Result<bool>;
}

struct Terminal;

impl Terminal {
    fn read(prompt: &str) -> Result<String> {
        print!("{prompt}");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        if std::io::stdin().lock().read_line(&mut answer)? == 0 {
            bail!("setup needs someone to answer its questions; run it in a terminal");
        }
        Ok(answer.trim().to_owned())
    }
}

impl Prompter for Terminal {
    fn line(&mut self, prompt: &str, default: Option<&str>) -> Result<String> {
        loop {
            let answer = Self::read(&match default {
                Some(default) => format!("{prompt} [{default}]: "),
                None => format!("{prompt}: "),
            })?;
            match (answer.is_empty(), default) {
                (false, _) => return Ok(answer),
                (true, Some(default)) => return Ok(default.to_owned()),
                (true, None) => {}
            }
        }
    }

    fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        loop {
            let answer = Self::read(&format!("{prompt} [{}]: ", if default { "Y/n" } else { "y/N" }))?;
            match answer.to_lowercase().as_str() {
                "" => return Ok(default),
                "y" | "yes" => return Ok(true),
                "n" | "no" => return Ok(false),
                _ => {}
            }
        }
    }

    fn secret(&mut self, prompt: &str) -> Result<Secret> {
        tty::read_password(prompt)
    }

    fn proceed(&mut self, prompt: &str) -> Result<bool> {
        loop {
            let answer = Self::read(&format!("{prompt} (or type s to skip): "))?;
            match answer.to_lowercase().as_str() {
                "" | "y" | "yes" => return Ok(true),
                "s" | "skip" | "n" | "no" => return Ok(false),
                _ => {}
            }
        }
    }
}

/// The console's address as typed, tidied: people paste web addresses.
/// Returns the host and a port if one was given.
fn console_address(typed: &str) -> Option<(String, Option<u16>)> {
    let text = typed.trim();
    let text = text.strip_prefix("https://").or_else(|| text.strip_prefix("http://")).unwrap_or(text);
    let text = text.split('/').next().unwrap_or(text);
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) => (host, Some(port.parse::<u16>().ok()?)),
        None => (text, None),
    };
    let plain = |c: char| c.is_ascii_alphanumeric() || ".-".contains(c);
    (!host.is_empty() && host.chars().all(plain)).then(|| (host.to_owned(), port))
}

/// A string as TOML writes it.
fn quoted(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

/// The `[protect]` section for a console that has none yet.
fn protect_section(host: &str, port: Option<u16>, username: &str, fingerprint: &Fingerprint) -> String {
    let port = port.filter(|port| *port != 443).map_or(String::new(), |port| format!("port = {port}\n"));
    format!(
        "[protect]\nhost = {}\n{port}username = {}\nfingerprint = {}\n",
        quoted(host),
        quoted(username),
        quoted(&fingerprint.to_string())
    )
}

/// `existing` with a fingerprint added to its `[protect]` section, which
/// has none. Everything else in the file is kept as it is.
fn with_fingerprint(existing: &str, fingerprint: &Fingerprint) -> Option<String> {
    let header = existing.lines().position(|line| line.trim() == "[protect]")?;
    let mut lines: Vec<String> = existing.lines().map(str::to_owned).collect();
    lines.insert(header + 1, format!("fingerprint = {}", quoted(&fingerprint.to_string())));
    Some(lines.join("\n") + "\n")
}

/// Replaces the config file in one step with one only its owner can read.
fn write_config(state_dir: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    // Never write a file the daemon would then refuse to start on.
    config::parse(text).context("the new config.toml would not be valid")?;
    let path = config::path(state_dir);
    let fresh = path.with_extension("toml.new");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&fresh)
        .with_context(|| format!("writing {}", fresh.display()))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&fresh, &path).with_context(|| format!("moving {} into place", path.display()))
}

/// What setup has learned about the console.
struct Console {
    host: String,
    port: u16,
    username: String,
    pin: Fingerprint,
    /// The text `config.toml` should have, if it is to change.
    config: Option<String>,
}

/// Asks where the console is and whose certificate it shows, or takes
/// both from a config that already says.
async fn find_console(state_dir: &Path, ask: &mut dyn Prompter) -> Result<Console> {
    let path = config::path(state_dir);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let configured = config::load(state_dir)?.protect;
    let (host, port, username) = match &configured {
        Some(protect) => {
            crate::account_lines::already_configured(&path, &protect.host, &protect.username);
            (protect.host.clone(), Some(protect.port), protect.username.clone())
        }
        None => {
            println!(
                "Gantry signs in to UniFi Protect with an account of its own. If you have not\n\
                 made one yet, do that first, in UniFi OS under Admins & Users:\n\n  \
                 - a local-access-only admin\n  \
                 - with access to Protect: Full Management lets Gantry switch floodlights,\n    \
                 View Only is enough for everything else\n"
            );
            let (host, port) = loop {
                let typed = ask.line("Address of your console, such as 192.168.1.1", None)?;
                match console_address(&typed) {
                    Some(address) => break address,
                    None => println!("That does not look like an address. An IP address such as 192.168.1.1 is what is wanted."),
                }
            };
            (host, port, ask.line("Username of the account you made for Gantry", Some("gantry"))?)
        }
    };
    let port_number = port.unwrap_or(443);

    if let Some(pinned) = configured.as_ref().and_then(|protect| protect.fingerprint.as_ref()) {
        let pin = pinned.parse().context("fingerprint in config.toml")?;
        return Ok(Console { host, port: port_number, username, pin, config: None });
    }
    let seen = fetch_fingerprint(&host, port_number)
        .await
        .with_context(|| format!("connecting to {host}:{port_number}"))?;
    println!(
        "\nThe console at {host} identifies itself with a certificate. Its fingerprint:\n\n{}\n\n\
         From now on Gantry only talks to a console that shows exactly this certificate,\n\
         so nothing else on the network can pose as yours. Nothing has been sent to it yet.\n\
         To check it, open https://{host} in a browser and look at the certificate's\n\
         SHA-256 fingerprint.\n",
        fingerprint_lines(&seen)
    );
    if !ask.confirm("Is that your console?", true)? {
        bail!("nothing was changed");
    }
    let config = match &configured {
        Some(_) => with_fingerprint(&existing, &seen).context("could not find [protect] in config.toml to add the fingerprint to")?,
        None => {
            let gap = if existing.is_empty() || existing.ends_with("\n\n") { "" } else { "\n" };
            format!("{existing}{gap}{}", protect_section(&host, port, &username, &seen))
        }
    };
    Ok(Console { host, port: port_number, username, pin: seen, config: Some(config) })
}

/// Signs in once to prove the account works. Returns the password that
/// did, and whether it came from the Keychain.
async fn sign_in(console: &Console, ask: &mut dyn Prompter) -> Result<(Secret, bool)> {
    let stored = if cfg!(target_os = "macos") {
        keychain::lookup(keychain::PROTECT_SERVICE, &console.username, Some(std::time::Duration::from_secs(5))).ok()
    } else {
        None
    };
    let mut candidate = stored;
    let mut from_keychain = candidate.is_some();
    for _ in 0..3 {
        let password = match candidate.take() {
            Some(password) => {
                println!("Using the password already in your Keychain.");
                password
            }
            None => {
                from_keychain = false;
                let typed = ask.secret(&format!("Password of the Protect account \"{}\" (hidden as you type): ", console.username))?;
                if typed.is_empty() {
                    bail!("no password was entered; nothing was changed");
                }
                typed
            }
        };
        let mut client = Client::new(&console.host, console.port, console.pin);
        match client.login(&console.username, password.expose()).await {
            Ok(()) => {
                let found = client.bootstrap().await?;
                println!(
                    "{}\n  Found {}, {} and {}.",
                    done(&format!(
                        "Signed in to {} ({}, Protect {}).",
                        found.nvr.name.as_deref().unwrap_or("the console"),
                        found.nvr.model,
                        found.nvr.version
                    )),
                    say::count(found.cameras.len(), "camera"),
                    say::count(found.lights.len(), "light"),
                    say::count(found.sensors.len(), "sensor")
                );
                return Ok((password, from_keychain));
            }
            Err(Error::BadCredentials) => println!("The console did not accept that username and password. Try again."),
            Err(other) => return Err(other).context("signing in to UniFi Protect"),
        }
    }
    bail!("the console rejected the password three times; nothing was changed")
}

/// Offers to keep the password in the Keychain, where `gantry
/// unlock` finds it.
fn keep_password(username: &str, password: &Secret, ask: &mut dyn Prompter) -> Result<()> {
    if !cfg!(target_os = "macos") {
        println!("There is no password store on this system yet, so `gantry unlock` will ask for the password.");
        return Ok(());
    }
    println!(
        "\nThe service asks for this password each time this Mac restarts. Gantry can keep it\n\
         in your Mac's login Keychain, so that you never have to type it again."
    );
    if !ask.confirm("Keep it in the Keychain?", true)? {
        println!("`gantry unlock` will ask for it instead.");
        return Ok(());
    }
    let mut outcome = keychain::store(keychain::PROTECT_SERVICE, username, password);
    if matches!(outcome, Err(Failure::Locked(_))) {
        println!("The Keychain is locked. macOS will now ask for your Mac login password to unlock it.");
        keychain::unlock_interactively()?;
        outcome = keychain::store(keychain::PROTECT_SERVICE, username, password);
        keychain::lock();
    }
    match outcome {
        Ok(()) => println!("{}", done("Stored in the Keychain.")),
        Err(why) => crate::account_lines::could_not_store(
            &why,
            &keychain::store_command(keychain::PROTECT_SERVICE, username),
        ),
    }
    Ok(())
}

/// Hands the password to a daemon that is waiting for it, if one is.
fn hand_over(state_dir: &Path, password: Secret, ask: &mut dyn Prompter) -> Result<()> {
    let Some(console) = client::state(state_dir).ok().and_then(|state| state.console) else {
        return Ok(());
    };
    // A service that read the password itself still has to get there.
    if !console.waiting_for_password {
        return until_connected(state_dir, ask);
    }
    match client::unlock(state_dir, password) {
        Ok(reply) if reply.signed_in => {
            println!("{}", done(&format!("The service is signed in, with {} ready for HomeKit.", say::count(reply.devices, "device"))));
        }
        Ok(reply) if reply.kept => {
            println!("The service has the password and is connecting to the console...");
            return until_connected(state_dir, ask);
        }
        Ok(reply) => println!("The service could not sign in: {}", reply.detail),
        Err(why) => println!("The password could not be handed to the service: {why:#}"),
    }
    Ok(())
}

/// What the owner is asked to do to start the service, step by step.
fn owner_instructions(command: &str, in_the_way: Option<u32>, over_ssh: bool) -> String {
    let window = if over_ssh {
        "Open a second terminal window and connect to this Mac in it, as you did here."
    } else {
        "Open a second terminal window. In Terminal that is Command-N."
    };
    let mut text = format!(
        "{}\n\
         This window waits while you do.\n\n  \
         1. {window}\n  \
         2. Copy the command between the two lines below and paste it there.\n  \
         3. Press Return, then type your Mac password when asked. Nothing shows\n     \
         as you type it.\n  \
         4. Come back to this window.\n\n{}\n",
        bold("To run the command yourself:"),
        say::copy_block(command)
    );
    if let Some(pid) = in_the_way {
        text.push_str(&format!("\n{}", service::make_way(pid)));
    }
    text
}

/// What is said before offering to run the administrator commands: what
/// they are, and where the password goes.
fn administrator_offer(steps: &[service::Step]) -> String {
    let commands: Vec<String> = steps.iter().map(|step| format!("    sudo {}", step.shell())).collect();
    format!(
        "{}\n\
         It takes administrator rights, so macOS will ask for your Mac password. The\n\
         password goes to macOS, never to Gantry. This is what will be run:\n\n{}\n",
        bold("One thing is left: starting the service."),
        commands.join("\n")
    )
}

/// Whether the service starts answering within its patience.
fn service_answers(state_dir: &Path) -> bool {
    let started = std::time::Instant::now();
    while started.elapsed() < SERVICE_PATIENCE {
        if client::state(state_dir).is_ok() {
            println!("{}", done("The service is running. It starts again whenever this Mac does."));
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Starts the service with the owner's leave: runs the administrator
/// commands here if they agree, and otherwise, or if that fails, shows
/// them to copy and waits. Says whether the service came up.
fn start_service(state_dir: &Path, command: &str, steps: &[service::Step], in_the_way: Option<u32>, ask: &mut dyn Prompter) -> Result<bool> {
    println!("\n{}", administrator_offer(steps));
    if ask.confirm("Run it now?", true)? {
        println!("\nType your Mac password if asked. Nothing shows as you type it.");
        match service::run_as_administrator(state_dir, steps, in_the_way) {
            Ok(()) if service_answers(state_dir) => return Ok(true),
            Ok(()) => println!("\nThe commands ran, but the service is not answering yet."),
            Err(why) => println!("\nThat did not work: {why:#}."),
        }
        // Whatever stood in the way has been stopped or was never the trouble.
        return owner_started_it(state_dir, command, None, ask);
    }
    owner_started_it(state_dir, command, in_the_way, ask)
}

/// Shows the owner the administrator command that is theirs to run and
/// waits while they do, then says whether the service came up. Setup
/// goes on without it when they would rather do that later.
fn owner_started_it(state_dir: &Path, command: &str, in_the_way: Option<u32>, ask: &mut dyn Prompter) -> Result<bool> {
    let over_ssh = std::env::var_os("SSH_CONNECTION").is_some();
    println!("\n{}", owner_instructions(command, in_the_way, over_ssh));
    loop {
        if !ask.proceed("Press Return here once the command has finished")? {
            println!("When you have run it, give the service the Protect password with:\n\n    gantry unlock\n");
            return Ok(false);
        }
        if service_answers(state_dir) {
            return Ok(true);
        }
        println!(
            "\nThe service is not answering yet. Look at the other window: if the command\n\
             printed an error, or is still waiting for your password, deal with that first."
        );
    }
}

/// How long a service just loaded gets to start answering.
const SERVICE_PATIENCE: Duration = Duration::from_secs(10);

/// How long setup watches the service reach the console before it says
/// what is in the way and asks.
const CONNECT_PATIENCE: Duration = Duration::from_secs(15);

/// Stays with the service until it has reached the console, saying what is
/// in the way and waiting for the person to deal with it. Leaving is
/// always possible: the service keeps trying on its own.
fn until_connected(state_dir: &Path, ask: &mut dyn Prompter) -> Result<()> {
    loop {
        let started = std::time::Instant::now();
        let problem = loop {
            let console = client::state(state_dir).ok().and_then(|state| state.console).unwrap_or_default();
            if console.connected {
                println!("{}", done("The service is signed in to the console."));
                return Ok(());
            }
            if started.elapsed() >= CONNECT_PATIENCE {
                break console.problem;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        println!("\n{}\n", not_connected(problem.as_deref()));
        if !ask.proceed("Press Return to check again")? {
            println!("The service keeps trying on its own. `gantry service status` says how it is doing.");
            return Ok(());
        }
    }
}

/// What to tell someone whose service has not reached the console.
fn not_connected(problem: Option<&str>) -> String {
    match problem {
        Some(LOCAL_NETWORK_BLOCKED) => format!(
            "{}\n\n\
             macOS has to be told that Gantry may talk to devices on your network. Look for a\n\
             dialog asking to let Gantry find devices on local networks and choose Allow. If there\n\
             is none, open System Settings, Privacy & Security, Local Network and switch Gantry on.\n\
             The service tries again every few seconds, so there is nothing to restart.",
            bold("The service cannot reach the console: it has no access to the local network.")
        ),
        Some(problem) => format!("The service has not reached the console yet: {problem}."),
        None => "The service has not reached the console yet.".to_string(),
    }
}

/// What setup opens with: whose questions these are, and how many.
fn banner() -> String {
    format!(
        "\n  {}\n  HomeKit bridge for UniFi Protect\n\n  \
         Setup is four steps: find your console, sign in, run as a service, add to Home.\n  \
         It takes a couple of minutes, and is safe to run again: nothing already in\n  \
         place is changed.\n",
        bold(&format!("Gantry {}", env!("CARGO_PKG_VERSION")))
    )
}

fn step(number: u8, title: &str) -> String {
    say::heading(&format!("Step {number} of 4 · {title}"))
}

/// A fingerprint on two lines, so that it fits a terminal of any width.
fn fingerprint_lines(fingerprint: &Fingerprint) -> String {
    let text = fingerprint.to_string();
    let bytes: Vec<&str> = text.split(':').collect();
    bytes.chunks(16).map(|half| format!("    {}", half.join(":"))).collect::<Vec<_>>().join("\n")
}

/// The pairing code of a running, unpaired bridge, drawn for the Home
/// app to scan. Nothing when the service is not up or is already paired.
fn pairing_code(state_dir: &Path) -> Option<String> {
    let state = client::state(state_dir).ok()?;
    let code = state.setup_code?;
    let picture = state.setup_uri.as_deref().and_then(crate::qr::render).map(|qr| format!("{qr}\n\n")).unwrap_or_default();
    Some(format!("{picture}Setup code: {code}"))
}

pub fn run(state_dir: &Path, user_agent: bool) -> Result<()> {
    let ask: &mut dyn Prompter = &mut Terminal;
    println!("{}", banner());
    daemon::create_private_dir(state_dir)?;
    let runtime = tokio::runtime::Runtime::new()?;

    println!("{}\n", step(1, "Find your console"));
    let console = runtime.block_on(find_console(state_dir, ask))?;

    println!("\n{}\n", step(2, "Sign in"));
    let (password, from_keychain) = runtime.block_on(sign_in(&console, ask))?;
    if let Some(text) = &console.config {
        write_config(state_dir, text)?;
        println!("{}", done(&format!("Saved your settings in {}.", config::path(state_dir).display())));
    }
    if !from_keychain {
        keep_password(&console.username, &password, ask)?;
    }
    if video::find_ffmpeg().is_none() {
        println!("\nLive video and recording need ffmpeg, which was not found. With Homebrew: brew install ffmpeg");
    }

    println!("\n{}\n", step(3, "Run as a service"));
    // A daemon started before the console was configured does not know of it.
    let stale = console.config.is_some() && client::state(state_dir).is_ok();
    println!(
        "As a service, Gantry starts whenever this Mac starts, with nobody logged in, and\n\
         is started again if it ever stops."
    );
    if ask.confirm("Install it as a service?", true)? {
        let options = service::Options {
            scope: if user_agent { service::Scope::User } else { service::Scope::System },
            listen: None,
            demo: false,
        };
        let installed = service::install_step(state_dir, &options)?;
        if stale {
            println!("The running daemon was started before this configuration. Restart it:\n\n    gantry service restart\n");
        } else {
            let running = match &installed {
                service::Installed::Running => true,
                service::Installed::AwaitsOwner { command, steps, in_the_way } => start_service(state_dir, command, steps, *in_the_way, ask)?,
            };
            if running {
                hand_over(state_dir, password, ask)?;
            }
        }
    } else {
        println!("To run it yourself: gantry daemon");
    }

    println!("\n{}\n", step(4, "Add it to Home"));
    match pairing_code(state_dir) {
        Some(code) => println!(
            "On your iPhone, open the Home app, tap + and choose Add Accessory. Then scan\n\
             this code, or choose More Options, pick Gantry and type the setup code.\n\n{code}\n\n\
             To see the code again, and everything else, open the dashboard:\n\n    gantry\n"
        ),
        None => println!(
            "Once the service is running and has the password (`gantry unlock`), open the\n\
             dashboard, which shows the pairing code:\n\n    gantry\n\n\
             Then on your iPhone open the Home app, tap + and choose Add Accessory. Scan the\n\
             code, or choose More Options and pick Gantry."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_addresses_are_tidied_and_nonsense_refused() {
        let address = |typed: &str| console_address(typed);
        assert_eq!(address(" 192.0.2.10 "), Some(("192.0.2.10".into(), None)));
        assert_eq!(address("https://unvr.example/protect/dashboard"), Some(("unvr.example".into(), None)));
        assert_eq!(address("192.0.2.10:8443"), Some(("192.0.2.10".into(), Some(8443))));
        for bad in ["", "   ", "my console", "host:port", "a\"b"] {
            assert_eq!(address(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_opening_names_the_program_and_the_steps_are_numbered() {
        let banner = banner();
        assert!(banner.contains(&format!("Gantry {}", env!("CARGO_PKG_VERSION"))), "{banner}");
        assert!(banner.contains("HomeKit bridge for UniFi Protect") && banner.contains("four steps"));
        // Tests do not run on a terminal, so nothing is styled.
        assert!(step(2, "Sign in").starts_with("── Step 2 of 4 · Sign in ─"));
        let fingerprint = fingerprint_lines(&pin());
        assert_eq!(fingerprint.lines().count(), 2, "{fingerprint}");
        assert_eq!(fingerprint.replace("\n    ", ":").trim(), pin().to_string());
        assert_eq!(pairing_code(Path::new("/nonexistent/gantry")), None);
    }

    fn pin() -> Fingerprint {
        Fingerprint::of(b"a certificate")
    }

    #[test]
    fn a_new_section_is_valid_and_keeps_what_was_there() {
        let section = protect_section("192.0.2.10", None, "harbor\"master", &pin());
        let protect = config::parse(&section).unwrap().protect.unwrap();
        assert_eq!((protect.host.as_str(), protect.port, protect.username.as_str()), ("192.0.2.10", 443, "harbor\"master"));
        assert_eq!(protect.fingerprint.unwrap().parse::<Fingerprint>().unwrap(), pin());
        let odd_port = protect_section("h", Some(8443), "u", &pin());
        assert_eq!(config::parse(&odd_port).unwrap().protect.unwrap().port, 8443);
        assert!(!protect_section("h", Some(443), "u", &pin()).contains("port"));
    }

    #[test]
    fn a_fingerprint_is_added_without_disturbing_the_rest() {
        let existing = "# mine\n[protect]\nhost = \"h\"\nusername = \"u\"\nname_prefix = \"HM \"\n";
        let text = with_fingerprint(existing, &pin()).unwrap();
        assert!(text.starts_with("# mine\n[protect]\nfingerprint = \""), "{text}");
        let protect = config::parse(&text).unwrap().protect.unwrap();
        assert_eq!((protect.name_prefix.as_str(), protect.fingerprint.is_some()), ("HM ", true));
        assert_eq!(with_fingerprint("nothing here\n", &pin()), None);
    }

    #[test]
    fn the_config_is_written_private_and_never_invalid() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hm-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(write_config(&dir, "[protect]\nhost = 3\n").is_err());
        assert!(!config::path(&dir).exists(), "a bad file is never put in place");
        write_config(&dir, &protect_section("192.0.2.10", None, "u", &pin())).unwrap();
        assert_eq!(std::fs::metadata(config::path(&dir)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(config::load(&dir).unwrap().protect.unwrap().host, "192.0.2.10");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_administrator_command_comes_with_what_to_do_with_it() {
        let told = owner_instructions("sudo /bin/true", None, false);
        assert!(told.contains("\n\nsudo /bin/true\n\n"), "the command stands on a line of its own: {told}");
        for wanted in ["Command-N", "Copy the command", "type your Mac password", "Come back to this window"] {
            assert!(told.contains(wanted), "{wanted}: {told}");
        }
        assert!(!told.contains("kill"));
        let offer = administrator_offer(&[service::Step::new(["/bin/true", "a b"]), service::Step::new(["/bin/false"])]);
        assert!(offer.contains("    sudo /bin/true 'a b'\n    sudo /bin/false\n"), "{offer}");
        assert!(offer.contains("never to Gantry"), "{offer}");
        let remote = owner_instructions("sudo /bin/true", Some(41), true);
        assert!(remote.contains("connect to this Mac") && !remote.contains("Command-N"), "{remote}");
        assert!(remote.contains("kill 41"), "{remote}");
    }

    #[test]
    fn a_blocked_local_network_is_explained_not_just_named() {
        let blocked = not_connected(Some(LOCAL_NETWORK_BLOCKED));
        assert!(blocked.contains("Local Network") && blocked.contains("choose Allow"), "{blocked}");
        assert!(blocked.contains("nothing to restart"), "{blocked}");
        let other = not_connected(Some("the console closed the connection"));
        assert!(other.contains("the console closed the connection") && !other.contains("Local Network"), "{other}");
        assert!(!not_connected(None).contains("Local Network"));
    }
}
