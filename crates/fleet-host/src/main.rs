//! fleet-host CLI: pair, run, status, unpair.

use anyhow::{Result, bail};
use fleet_host::run;

enum Cmd {
    Pair { url: String, token: String },
    Run,
    Update,
    Status,
    Unpair,
    Help,
    Version,
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("fleet-host: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    match parse_args(std::env::args().collect())? {
        Cmd::Help => {
            run::print_usage();
            Ok(())
        }
        Cmd::Version => {
            println!("fleet-host {}", env!("CARGO_PKG_VERSION"));
            match fleet_host::software::baked_git_sha() {
                Some(sha) => println!("git_sha {sha}"),
                None => println!("git_sha unknown"),
            }
            Ok(())
        }
        Cmd::Status => run::status(),
        Cmd::Unpair => run::unpair(),
        Cmd::Pair { url, token } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run::pair(&url, &token))
        }
        Cmd::Run => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run::run())
        }
        Cmd::Update => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(run::update())
        }
    }
}

fn parse_args(args: Vec<String>) -> Result<Cmd> {
    let mut it = args.into_iter().skip(1);
    let Some(cmd) = it.next() else {
        run::print_usage();
        std::process::exit(2);
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => Ok(Cmd::Help),
        "-V" | "--version" | "version" => Ok(Cmd::Version),
        "status" => Ok(Cmd::Status),
        "unpair" => Ok(Cmd::Unpair),
        "run" => Ok(Cmd::Run),
        "update" => Ok(Cmd::Update),
        "pair" => parse_pair(it.collect()),
        other => {
            bail!("unknown command: {other}\n\nSee `fleet-host --help`");
        }
    }
}

fn parse_pair(args: Vec<String>) -> Result<Cmd> {
    let mut url = None;
    let mut token = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "-h" || a == "--help" {
            return Ok(Cmd::Help);
        }
        if let Some(v) = a.strip_prefix("--url=") {
            url = Some(v.to_string());
        } else if a == "--url" {
            i += 1;
            url = Some(
                args.get(i)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("pair --url needs a value"))?,
            );
        } else if let Some(v) = a.strip_prefix("--token=") {
            token = Some(v.to_string());
        } else if a == "--token" {
            i += 1;
            token = Some(
                args.get(i)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("pair --token needs a value"))?,
            );
        } else {
            bail!("unknown pair argument: {a}");
        }
        i += 1;
    }
    let url = url.ok_or_else(|| anyhow::anyhow!("pair requires --url URL"))?;
    let token = token.ok_or_else(|| anyhow::anyhow!("pair requires --token TOKEN"))?;
    if url.trim().is_empty() {
        bail!("--url is empty");
    }
    if token.trim().is_empty() {
        bail!("--token is empty");
    }
    Ok(Cmd::Pair { url, token })
}
