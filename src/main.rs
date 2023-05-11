use serde::Deserialize;
use reqwest::{self, Client, Url};
use semver::Version;
use bunt::{println as bprintln, eprintln as ebprintln};
use tokio::{fs, runtime::Builder, io::AsyncWriteExt};
use join::try_async_spawn;
use MyExit::*;
use anyhow::{Context, Result, anyhow, Error as AnyError};
use std::{
    path::{Path, PathBuf},
    process::{ExitCode, Termination},
    os::unix::prelude::PermissionsExt
};

const NVIM_VERSION_PATH: &str = "/opt/neovim/current_version";
const NVIM_PATH: &str = "/opt/neovim/nvim.appimage";
const NVIM_API: &str = "https://api.github.com/repos/neovim/neovim/releases/latest";

enum MyExit {
    Success,
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

macro_rules! fail {
    ($($arg:tt)*) => {
        return MyExit::Fail(anyhow!($($arg)*))
    };
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

async fn get_current(read_file: bool, version_path: PathBuf) -> Result<Version> {
    if read_file {
        Version::parse(&fs::read_to_string(version_path).await
            .with_context(|| format!("Could not access '{NVIM_VERSION_PATH}'"))?)
    } else {
        Version::parse("0.0.0")
    }.context("Could not parse current nvim version")
}

async fn get_latest(client: Client) -> Result<(Version, Url)> {
    bprintln!("{$green}Polling {$bold}Neovim{/$} github releases API...{/$}");
    let nvim_res: NvimResponse = client
        .get(NVIM_API).header("User-Agent", "request")
        .send().await.context("Request failed")?
        .json().await.context("JSON conversion failed")?;
    
    let version = Version::parse(nvim_res.body
        .lines().nth(1).ok_or_else(|| anyhow!("Could not get second line of 'body'"))?
        .split(' ').nth(1).ok_or_else(|| anyhow!("Could not get second segment of second line of 'body'"))?
        .strip_prefix('v').ok_or_else(|| anyhow!("Could not strip 'v' from segment"))?)
        .context("Failed to parse version from 'body'")?;
    
    let down_url = Url::parse(&nvim_res.assets
        .into_iter().find(|a| a.content_type == "application/vnd.appimage")
        .ok_or_else(|| anyhow!("Could not find correct asset"))?
        .browser_download_url)
        .context("Failed to parse Url from JSON")?;
    
    Ok((version, down_url))
}

async fn do_upgrade(client: Client, down_url: Url) -> Result<()> {
    let bytes = client.get(down_url)
        .send().await.context("Download GET request failed")?
        .bytes().await.context("Could not get bytes from response body")?;
    bprintln!("{$green}Installing new version...{/$}");
    let mut nvim_file = fs::OpenOptions::new().create(true).write(true).open(NVIM_PATH)
        .await.with_context(|| format!("Failed to open '{NVIM_PATH}'"))?;
    nvim_file.write_all(&bytes).await.with_context(|| format!("Failed to write to '{NVIM_PATH}':"))?;
    nvim_file.set_permissions({
        let mut perms = nvim_file
            .metadata().await.with_context(|| format!("Could not get metadata from '{NVIM_PATH}'"))?
            .permissions();
        perms.set_mode(0o755);
        perms
    }).await.with_context(|| format!("Failed to set file permissions for '{NVIM_PATH}'"))
}

async fn run(client: Client, version_path: PathBuf, read_file: bool) -> Result<()> {
    let c_handle = get_current(read_file, version_path);
    let l_handle = get_latest(client.clone());
    let (current, (latest, down_url)) = try_async_spawn!(c_handle, l_handle).await?;
    
    Ok(match latest.cmp(&current) {
        std::cmp::Ordering::Equal => bprintln!("{$green}{$bold}Neovim{/$} is up to date!{/$} {$dimmed}(v{}){/$}", current),
        std::cmp::Ordering::Greater => {
            bprintln!("{$green}Downloading latest version...{/$} {$dimmed}(v{}){/$}", latest);
            do_upgrade(client, down_url).await?;
            fs::write(NVIM_VERSION_PATH, latest.to_string()).await
                .with_context(|| format!("Failed to write new version to '{NVIM_VERSION_PATH}'"))?;
            bprintln!("{$green}Done!{/$}")
        },
        _ => Err(anyhow!("How did you get a newer version than the latest?"))?,
    })
}

fn main() -> MyExit {
    let runtime = match Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r, Err(ex) => fail!("Runtime building failed: {ex}")
    };

    let client = Client::new();
    let version_path = PathBuf::from(NVIM_VERSION_PATH);
    let read_file = {
        let tmp = !Path::new(NVIM_PATH).exists() || !version_path.exists();
        if tmp { bprintln!("{$yellow+bold}No (valid) Neovim Installation Found.{/$}"); }
        !tmp
    };

    match runtime.block_on(run(client, version_path, read_file)) {
        Err(ex) => Fail(ex), _ => Success
    }
}