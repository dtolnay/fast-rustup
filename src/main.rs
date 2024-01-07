#![allow(clippy::let_unit_value)]

use anyhow::bail;
use bytes::{Buf as _, Bytes};
use clap::Parser;
use rayon::{ThreadPool, ThreadPoolBuilder};
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tar::EntryType;
use target_triple::target;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use url::Url;

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const USER_AGENT: &str = concat!("dtolnay/fast-rustup/v", env!("CARGO_PKG_VERSION"));
const RUSTUP_DIST_SERVER: &str = "https://static.rust-lang.org";
const TARGET: &str = target!();

struct Component {
    archive: &'static str,
    subdir: &'static str,
}

const COMPONENTS: &[Component] = &[
    Component {
        archive: concat!("cargo-nightly-", target!(), ".tar.xz"),
        subdir: "cargo",
    },
    Component {
        archive: concat!("clippy-nightly-", target!(), ".tar.xz"),
        subdir: "clippy-preview",
    },
    Component {
        archive: concat!("rust-docs-nightly-", target!(), ".tar.xz"),
        subdir: "rust-docs",
    },
    Component {
        archive: concat!("rust-std-nightly-", target!(), ".tar.xz"),
        subdir: concat!("rust-std-", target!()),
    },
    Component {
        archive: concat!("rustc-nightly-", target!(), ".tar.xz"),
        subdir: "rustc",
    },
    Component {
        archive: concat!("rustfmt-nightly-", target!(), ".tar.xz"),
        subdir: "rustfmt-preview",
    },
];

#[derive(clap::Parser)]
#[command(version, author)]
struct Cli {
    #[arg(
        value_name = "nightly-2024-01-01",
        default_value = "nightly-2024-01-01"
    )]
    nightly: String,
}

fn main() -> anyhow::Result<()> {
    let begin = Instant::now();

    do_main()?;

    let elapsed = begin.elapsed();
    let _ = writeln!(io::stderr(), "elapsed: {:.03?} sec", elapsed.as_secs_f64());
    Ok(())
}

fn do_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let date = if cli.nightly.starts_with("nightly-")
        && cli.nightly.len() == "nightly-2024-01-01".len()
        && cli.nightly[8..12].bytes().all(|b| b.is_ascii_digit())
        && cli.nightly[12..13] == *"-"
        && cli.nightly[13..15].bytes().all(|b| b.is_ascii_digit())
        && cli.nightly[15..16] == *"-"
        && cli.nightly[16..18].bytes().all(|b| b.is_ascii_digit())
    {
        &cli.nightly["nightly-".len()..]
    } else {
        bail!(
            "{:?}: expected a nightly version in the form \"nightly-2024-01-01\"",
            cli.nightly,
        );
    };

    let mut root = home::rustup_home()?;
    create_dir_if_not_exists(&root)?;
    root.push("toolchains");
    create_dir_if_not_exists(&root)?;
    root.push(format!("nightly-{date}-{TARGET}"));
    if root.try_exists()? {
        bail!("toolchain already exists: {}", root.display());
    }

    let _ = writeln!(io::stderr(), "Downloading nightly-{date} for {TARGET}");

    let num_threads = thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(COMPONENTS.len());
    let thread_pool = ThreadPoolBuilder::new().num_threads(num_threads).build()?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(do_install(thread_pool, date, &root))
}

fn create_dir_if_not_exists(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

struct Chunks {
    cur: bytes::buf::Reader<Bytes>,
    rest: UnboundedReceiver<Bytes>,
}

impl Chunks {
    fn new(receiver: UnboundedReceiver<Bytes>) -> Self {
        Chunks {
            cur: Bytes::new().reader(),
            rest: receiver,
        }
    }
}

impl io::Read for Chunks {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.cur.get_ref().is_empty() {
            match self.rest.blocking_recv() {
                Some(chunk) => self.cur = chunk.reader(),
                None => return Ok(0),
            }
        }
        io::Read::read(&mut self.cur, buf)
    }
}

async fn do_install(thread_pool: ThreadPool, date: &str, root: &Path) -> anyhow::Result<()> {
    let (complete_sender, mut complete_receiver) = mpsc::unbounded_channel();
    let mut task_handles = Vec::new();

    let http_client = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
    let http_client = Arc::new(http_client);

    for component in COMPONENTS {
        let url_string = format!(
            "{RUSTUP_DIST_SERVER}/dist/{date}/{archive}",
            archive = component.archive,
        );
        let url = Url::parse(&url_string)?;

        let (chunk_sender, chunk_receiver) = mpsc::unbounded_channel();

        task_handles.push(tokio::spawn({
            let http_client = Arc::clone(&http_client);
            async move {
                let req = http_client.get(url);
                let mut resp = req.send().await?;
                let status = resp.status();
                if !status.is_success() {
                    bail!("{} {}", status, url_string);
                }
                while let Some(chunk) = resp.chunk().await? {
                    if chunk_sender.send(chunk).is_err() {
                        break;
                    }
                }
                drop(chunk_sender);
                Ok(())
            }
        }));

        thread_pool.spawn({
            let root = root.to_owned();
            let complete_sender = complete_sender.clone();
            move || {
                let result = do_extract(&root, chunk_receiver, component.subdir);
                let _ = complete_sender.send(result);
            }
        });
    }

    drop(complete_sender);

    while let Some(result) = complete_receiver.recv().await {
        () = result?;
    }

    for task_handle in task_handles {
        task_handle.await??;
    }

    Ok(())
}

fn do_extract(
    root: &Path,
    receiver: UnboundedReceiver<Bytes>,
    subdir: &'static str,
) -> anyhow::Result<()> {
    let chunks = Chunks::new(receiver);
    let xz = xz2::read::XzDecoder::new(chunks);
    let mut archive = tar::Archive::new(xz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let header = entry.header();
        let path = entry.path()?;
        let mut components = path.components();
        if components.next().is_none() {
            continue;
        }
        match components.next() {
            Some(component) if component.as_os_str() == subdir => {}
            _ => continue,
        }
        if components.as_path().as_os_str() == "manifest.in" {
            continue;
        }
        let target = root.join(components);
        if let EntryType::Directory = header.entry_type() {
            if let Err(err) = fs::create_dir(&target) {
                if err.kind() != ErrorKind::AlreadyExists {
                    bail!(err);
                }
            }
        } else {
            entry.unpack(target)?;
        }
    }
    Ok(())
}
