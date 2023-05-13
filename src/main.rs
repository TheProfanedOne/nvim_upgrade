use MyExit::*;
use serde::Deserialize;
use reqwest::{Client, Url};
use semver::Version;
use bunt::{println as bprintln, eprintln as ebprintln};
use tokio::{fs, runtime::{Builder, Runtime}, io::AsyncWriteExt};
use join::{try_async_spawn, try_spawn, try_join, join};
use partial_application::partial;
use anyhow::{Context, Result, Error as AnyError, anyhow};
use indicatif::{ProgressBar, ProgressStyle};
use futures_util::StreamExt;
use once_cell::sync::OnceCell;
use std::{
    cmp::{Ordering::*, min}, path::Path,
    process::{ExitCode, Termination},
    os::unix::prelude::PermissionsExt
};

const VERSION: &str = "/opt/neovim/current_version";
const APP_PATH: &str = "/opt/neovim/nvim.appimage";
const NVIM_API: &str = "https://api.github.com/repos/neovim/neovim/releases/latest";
static CLIENT: OnceCell<Client> = OnceCell::new();

fn get_client() -> Result<&'static Client> {
    CLIENT.get().ok_or_else(|| anyhow!("Failed to access CLIENT."))
}

enum MyExit {
    Success(()),
    Fail(AnyError)
}

impl Termination for MyExit {
    fn report(self) -> ExitCode {
        if let Self::Fail(e) = self {
            ebprintln!("{[red+bold]:?}", e);
            ExitCode::FAILURE
        } else { ExitCode::SUCCESS }
    }
}

#[derive(Deserialize)]
struct NvimAsset {
    content_type: String,
    browser_download_url: String
}

#[derive(Deserialize)]
struct NvimResponse {
    assets: Vec<NvimAsset>,
    body: String
}

macro_rules! gen_ctx { ($path:expr) => {
    format!("Failed to access '{}'", $path)
}}

async fn get_current(read_file: bool) -> Result<Version> {
    if read_file {
        fs::read_to_string(VERSION).await.with_context(|| gen_ctx!(VERSION))?.as_str().parse()
    } else {
        "0.0.0".parse()
    }.context("Failed to parse current nvim version")
}

async fn get_latest() -> Result<(Version, Url)> {
    bprintln!("{$green}Polling {$bold}Neovim{/$} GitHub releases API...{/$}");
    let res: NvimResponse = get_client()?
        .get(NVIM_API).header("User-Agent", "request")
        .send().await.context("JSON Request Failed")?
        .json().await.context("JSON Conversion Failed")?;

    let version = res.body
        .lines().nth(1).ok_or_else(|| anyhow!("Could not get second line of 'body'"))?
        .split(' ').nth(1).ok_or_else(|| anyhow!("Could not get second segment of second line of 'body'"))?
        .strip_prefix('v').ok_or_else(|| anyhow!("Could not strip 'v' from segment"))?
        .parse().context("Failed to parse version from 'body'")?;

    let down_url = res.assets
        .into_iter().find(|a| a.content_type == "application/vnd.appimage")
        .ok_or_else(|| anyhow!("Could not find correct asset"))?
        .browser_download_url.as_str().parse()
        .context("Failed to parse Url from JSON")?;

    Ok((version, down_url))
}

async fn do_upgrade(down_url: Url) -> Result<()> {
    let res = get_client()?.get(down_url).send().await.context("Download GET request failed")?;
    let total_size = res.content_length().ok_or_else(|| anyhow!("Failed to get size of response body."))?;

    let pb = ProgressBar::new(total_size).with_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:50.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
        .context("Error while downloading `nvim.appimage`")?
        .progress_chars("#>-"));

    let mut file = fs::OpenOptions::new().create(true).write(true).open(APP_PATH)
        .await.with_context(|| gen_ctx!(APP_PATH))?;
    let mut downloaded = 0;
    let mut stream = res.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item.context("Error while downloading 'nvim.appimage'")?;
        file.write_all(&chunk).await.with_context(|| format!("Error while writing to '{APP_PATH}'"))?;
        let new = min(downloaded + (chunk.len() as u64), total_size);
        downloaded = new;
        pb.set_position(new);
    }

    pb.finish();

    file.set_permissions({
        let mut perms = file
            .metadata().await.with_context(|| format!("Could not get metadata from '{APP_PATH}'"))?
            .permissions();
        perms.set_mode(0o755);
        perms
    }).await.with_context(|| format!("Failed to set file permissions for '{APP_PATH}'"))
}

async fn run(read_file: bool) -> Result<()> {
    let (current, (latest, down_url)) = try_async_spawn!(read_file -> get_current, get_latest()).await?;

    match latest.cmp(&current) {
        Equal => Ok(bprintln!("{$green}{$bold}Neovim{/$} is up to date!{/$} {$dimmed}(v{}){/$}", current)),
        Greater => {
            bprintln!("{$green}Downloading latest version...{/$} {$dimmed}(v{}){/$}", latest);
            do_upgrade(down_url).await?;
            fs::write(VERSION, latest.to_string()).await
                .with_context(|| format!("Failed to write new version to '{VERSION}'"))?;
            Ok(bprintln!("{$green}Done!{/$}"))
        },
        _ => Err(anyhow!("How did you get a newer version than the latest?"))
    }
}

fn check_files() -> Result<bool> {
    let [res1, res2] = [APP_PATH, VERSION].map(|p| Path::new(p)
        .try_exists()
        .with_context(|| format!("Failed to access '{p}'")));

    try_spawn!(res1, res2).map(|t| if t.0 && t.1 { true } else {
        bprintln!("{$yellow+bold}No (valid) Neovim Installation Found.{/$}");
        false
    })
}

fn async_handle(rt: Runtime) -> Result<()> {
    try_join! {
        Client::builder()
        >. build()
        >. context("Failed to build client")
        => >>> -> partial!(OnceCell::set => &CLIENT, _)
        !> |_| anyhow!("Failed to initialize CLIENT")
    }?;
    
    rt.block_on(run(check_files()?))
}

fn main() -> MyExit {
    join! {
        Builder::new_multi_thread()
        >. enable_all()
        >. build()
        >. context("Failed to build runtime")
        => async_handle
        >. map_or_else(Fail, Success)
    }
}
