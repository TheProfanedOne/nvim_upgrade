use MyExit::*;
use serde::Deserialize;
use reqwest::{self, Client, Url};
use semver::Version;
use bunt::{println as bprintln, eprintln as ebprintln};
use tokio::{fs, runtime::Builder, io::AsyncWriteExt};
use join::try_async_spawn;
use anyhow::{Context, Result, Error as AnyError, anyhow, bail};
use indicatif::{ProgressBar, ProgressStyle};
use futures_util::StreamExt;
use std::{
    cmp::{Ordering::*, min}, path::Path,
    process::{ExitCode, Termination},
    os::unix::prelude::PermissionsExt
};

const VERSION: &str = "/opt/neovim/current_version";
const APP_PATH: &str = "/opt/neovim/nvim.appimage";
const NVIM_API: &str = "https://api.github.com/repos/neovim/neovim/releases/latest";

enum MyExit {
    Success(()),
    Fail(AnyError)
}

impl Termination for MyExit {
    fn report(self) -> ExitCode {
        if let Self::Fail(msg) = self {
            ebprintln!("{[red+bold]}", msg);
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

async fn get_current(read_file: bool) -> Result<Version> {
    if read_file {
        Version::parse(&fs::read_to_string(VERSION).await
            .with_context(|| format!("Could not access '{VERSION}'"))?)
    } else {
        Version::parse("0.0.0")
    }.context("Could not parse current nvim version")
}

async fn get_latest(client: Client) -> Result<(Version, Url)> {
    bprintln!("{$green}Polling {$bold}Neovim{/$} github releases API...{/$}");
    let res: NvimResponse = client
        .get(NVIM_API).header("User-Agent", "request")
        .send().await.context("Request failed")?
        .json().await.context("JSON conversion failed")?;

    let version = Version::parse(res.body
        .lines().nth(1).ok_or_else(|| anyhow!("Could not get second line of 'body'"))?
        .split(' ').nth(1).ok_or_else(|| anyhow!("Could not get second segment of second line of 'body'"))?
        .strip_prefix('v').ok_or_else(|| anyhow!("Could not strip 'v' from segment"))?)
        .context("Failed to parse version from 'body'")?;

    let down_url = Url::parse(&res.assets
        .into_iter().find(|a| a.content_type == "application/vnd.appimage")
        .ok_or_else(|| anyhow!("Could not find correct asset"))?
        .browser_download_url)
        .context("Failed to parse Url from JSON")?;

    Ok((version, down_url))
}

async fn do_upgrade(client: Client, down_url: Url) -> Result<()> {
    let res = client.get(down_url).send().await.context("Download GET request failed")?;
    let total_size = res.content_length().ok_or_else(|| anyhow!("Failed to get size of response body."))?;

    let pb = ProgressBar::new(total_size).with_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:50.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")
        .context("Error while downloading `nvim.appimage`")?
        .progress_chars("#>-"));

    let mut file = fs::OpenOptions::new().create(true).write(true).open(APP_PATH)
        .await.with_context(|| format!("Failed to open '{APP_PATH}'"))?;
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

async fn run(client: Client, read_file: bool) -> Result<()> {
    let c_handle = get_current(read_file);
    let l_handle = get_latest(client.clone());
    let (current, (latest, down_url)) = try_async_spawn!(c_handle, l_handle).await?;

    Ok(match latest.cmp(&current) {
        Equal => bprintln!("{$green}{$bold}Neovim{/$} is up to date!{/$} {$dimmed}(v{}){/$}", current),
        Greater => {
            bprintln!("{$green}Downloading latest version...{/$} {$dimmed}(v{}){/$}", latest);
            do_upgrade(client, down_url).await?;
            fs::write(VERSION, latest.to_string()).await
                .with_context(|| format!("Failed to write new version to '{VERSION}'"))?;
            bprintln!("{$green}Done!{/$}")
        },
        _ => bail!("How did you get a newer version than the latest?")
    })
}

fn main() -> MyExit {
    let create_runtime = || Builder::new_multi_thread().enable_all().build().context("Runtime building failed");
    let check_files = || [APP_PATH, VERSION].map(|p| Path::new(p).try_exists().with_context(|| format!("Failed to access '{p}'")));
    create_runtime().and_then(|rt| rt.block_on(run(Client::new(), match check_files() {
        [Err(e), _] | [_, Err(e)] => return Err(e),
        [Ok(true), Ok(true)] => true,
        _ => {
            bprintln!("{$yellow+bold}No (valid) Neovim Installation Found.{/$}");
            false
        }
    }))).map_or_else(Fail, Success)
}
