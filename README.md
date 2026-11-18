# Gantry

A HomeKit bridge for UniFi Protect, in one small program.

Gantry publishes the cameras, doorbells, sensors and floodlights on a
UniFi Protect console to Apple Home. It is written in Rust, runs as a
background service, and has a terminal dashboard you can attach from
anywhere you can SSH.

**Status: a proof of concept.** It runs one household's cameras day to day,
on macOS on Apple silicon. Linux is next. Expect rough edges.

## What it does

- **Live view** with sound, started from the camera's native feed, so it
  opens in about a quarter of a second.
- **HomeKit Secure Video**: recordings to iCloud with your home hub's
  person, animal, vehicle and package analysis.
- **Two-way talk** on doorbells and cameras with a speaker.
- **Doorbell** presses, and a separate accessory for a doorbell's package
  camera.
- **Motion** from every camera, and optional **occupancy sensors** for what
  Protect itself recognises (person, vehicle, animal, package), for
  automations and notifications.
- **Sensors**: contact, motion, temperature, humidity, light level, leak,
  battery and tamper.
- **Floodlights**: on, off and brightness.
- A **dashboard** in the terminal: what is online, who is watching, what is
  recording, the log, and the settings below.

## What you need

- A Mac running macOS 14 or later. Developed and tested on Apple silicon
  with macOS 15; the Intel build is provided but untested.
- A UniFi Protect console on the same network.
- [ffmpeg](https://ffmpeg.org), for video. With Homebrew: `brew install ffmpeg`.
- For recordings, a home hub (Apple TV or HomePod) and an iCloud+ plan, as
  for any HomeKit Secure Video camera.

## Install

With Homebrew:

```sh
brew install ultradotdev/tap/gantry
```

Or with the install script, which checks the download against its
published checksum:

```sh
curl -fsSL https://raw.githubusercontent.com/ultradotdev/gantry/main/install.sh | sh
```

Or from source, with a recent Rust toolchain:

```sh
cargo build --release
```

## Set up

```sh
gantry setup
```

It asks for the console's address and an account, shows you the
certificate the console presents so you can confirm it is yours, signs in
once to prove the account works, and installs the service. Starting the
service takes administrator rights: setup shows the commands and offers to
run them, and macOS asks for your Mac password. You can also run them
yourself.

Create an account for Gantry in UniFi OS first: a local-access-only
admin with access to Protect. Full management lets Gantry switch
floodlights; view only is enough for everything else.

Then open the dashboard, which shows the pairing code, and add it in the
Home app under Add Accessory, More Options.

```sh
gantry
```

## Day to day

| Command | What it does |
|---|---|
| `gantry` | Open the dashboard |
| `gantry unlock` | Give the service the Protect password after a restart |
| `gantry service status` | What is installed and running |
| `gantry service restart` | Restart the service |
| `gantry service stop` / `start` | Stop it until started again |
| `gantry logs -f` | Follow the log |
| `gantry service uninstall` | Remove the service |

In the dashboard: arrows or the mouse to select, `s` to share or hide a
device, `r` to rename it, `P` and `S` for a prefix or suffix on every name,
`1` to `4` for person, vehicle, animal and package sensors (on the overview
for every camera, on a camera for that one), `R` to look for new devices
now, `q` to quit.

After upgrading with Homebrew, run `gantry service install` once more
so the service picks up the new version.

## How it handles your password

The service runs from boot, before anyone logs in, where it cannot read
your Keychain. So by default it does not store the Protect password: after
a restart it waits until you run `gantry unlock`, which reads the
password from your Keychain (or asks for it) and hands it over. The service
keeps it in memory only.

For a machine that must come back with nobody there, you can instead put
the password in a file named `protect-password`, readable only by you, in
the state folder. That trades some safety for convenience.

`config.toml` holds no secrets. Gantry only talks to a console that
presents the exact certificate you confirmed during setup.

## Things to know

- **Local Network permission.** The first time the service runs, macOS may
  block it from reaching the console ("No route to host"). Allow
  Gantry under System Settings, Privacy & Security, Local Network.
  Setup says so and waits while you do; the service picks it up within
  seconds, with no restart.
- **New devices.** A device you adopt in Protect appears in the Home app
  on its own within five minutes. `R` in the dashboard looks for it right
  away.
- **After an update**, the first connection attempt can fail and the retry
  a moment later succeeds. `gantry unlock` waits for that.
- **Names.** Home keeps the name an accessory had when it was added.
  Renaming in the dashboard changes what new accessories are called.
- Chimes, smoke and CO alarms, and Protect's sound detections are not
  published.

## Files

Everything lives in `~/Library/Application Support/gantry`:
`config.toml`, the HomeKit pairing, your dashboard choices, the log, and the
copy of the program the service runs. Deleting that folder after
`gantry service uninstall` removes Gantry entirely.

## Thanks

To HJD for [unifi-protect](https://github.com/hjdhjd/unifi-protect) and to
the [HAP-NodeJS](https://github.com/homebridge/HAP-NodeJS) contributors.
Gantry shares no code with either, and would not exist without what
they worked out and documented. See [NOTICE](NOTICE).

## License

Apache-2.0. See [LICENSE](LICENSE).

Gantry is not affiliated with or endorsed by Apple or Ubiquiti.
