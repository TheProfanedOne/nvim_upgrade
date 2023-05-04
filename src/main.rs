use serde::Deserialize;
use reqwest::{self, Client};
use semver::Version;
use libc::{chmod as sys_chmod, mode_t};
use bunt::{println as bprintln, eprintln as ebprintln};
use tokio::{fs, runtime::Builder};
use join::{try_async_spawn, try_join_async};
use MyExit::*;
use std::{
    ffi::CString, path::{Path, PathBuf},
    process::{ExitCode, Termination}
};

const NVIM_VERSION_PATH: &str = "/opt/neovim/current_version";
const NVIM_PATH: &str = "/opt/neovim/nvim.appimage";
const NVIM_API: &str = "https://api.github.com/repos/neovim/neovim/releases/latest";

enum MyExit {
    Success,
    Fail(String)
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
        return MyExit::Fail(format!($($arg)*))
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

fn chmod(path: &str, mode: mode_t) -> Result<(), String> {
    let path = CString::new(path).map_err(|ex| format!("Could not convert path to CString: {ex}"))?;
    let res = unsafe { sys_chmod(path.as_ptr(), mode) };
    if res == 0 { Ok(()) } else { Err(format!("chmod failed with error code {res}")) }
}

async fn get_current(read_file: bool, version_path: PathBuf) -> Result<Version, String> {
    if read_file {
        Version::parse(&try_join_async! {
            fs::read_to_string(version_path) !> |ex| format!("Could not access '{NVIM_VERSION_PATH}': {ex}")
        }.await?)
    } else {
        Version::parse("0.0.0")
    }.map_err(|ex| format!("Could not parse current nvim version: {ex}"))
}

async fn get_latest(client: Client) -> Result<(Version, String), String> {
    bprintln!("{$green}Polling {$bold}Neovim{/$} github releases API...{/$}");
    let nvim_res: NvimResponse = client
        .get(NVIM_API).header("User-Agent", "request")
        .send().await.map_err(|ex| format!("Request failed: {ex}"))?
        .json().await.map_err(|ex| format!("JSON conversion failed: {ex}"))?;
    
    let version = Version::parse(nvim_res.body
        .lines().nth(1).ok_or("Could not get second line of 'body'")?
        .split(' ').nth(1).ok_or("Could not get second segment of second line of 'body'")?
        .strip_prefix('v').ok_or("Could not strip 'v' from segment")?)
        .map_err(|ex| format!("Failed to parse version from 'body': {ex}"))?;
    
    let down_url = nvim_res.assets
        .into_iter().find(|a| a.content_type == "application/vnd.appimage")
        .ok_or("Could not find correct asset")?
        .browser_download_url;
    
    Ok((version, down_url))
}

async fn do_upgrade(client: Client, down_url: String) -> Result<(), String> {
    let res = client.get(down_url).send().await.map_err(|ex| format!("Download GET request failed: {ex}"))?;
    let bytes = res.bytes().await.map_err(|ex| format!("Could not get bytes from response body: {ex}"))?;
    bprintln!("{$green}Installing new version...{/$}");
    fs::write(NVIM_PATH, bytes).await.map_err(|ex| format!("Failed to write to '{NVIM_PATH}': {ex}"))?;
    chmod(NVIM_PATH, 0o755).map_err(|ex| format!("Could not call chmod on '{NVIM_PATH}': {ex}"))
}

async fn run(client: Client, version_path: PathBuf, read_file: bool) -> Result<(), String> {
    let c_handle = get_current(read_file, version_path);
    let l_handle = get_latest(client.clone());
    let (current, (latest, down_url)) = try_async_spawn!(c_handle, l_handle).await?;
    
    Ok(match latest.cmp(&current) {
        std::cmp::Ordering::Equal => bprintln!("{$green}{$bold}Neovim{/$} is up to date!{/$} {$dimmed}(v{}){/$}", current),
        std::cmp::Ordering::Greater => {
            bprintln!("{$green}Downloading latest version...{/$} {$dimmed}(v{}){/$}", latest);
            do_upgrade(client, down_url).await?;
            fs::write(NVIM_VERSION_PATH, latest.to_string()).await
                .map_err(|ex| format!("Failed to write new version to '{NVIM_VERSION_PATH}': {ex}"))?;
            bprintln!("{$green}Done!{/$}")
        },
        _ => Err("How did you get a newer version than the latest?")?,
    })
}

fn main() -> MyExit {
    let runtime = match Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt, Err(ex) => fail!("Runtime building failed: {ex}")
    };

    let client = Client::new();
    let version_path = PathBuf::from(NVIM_VERSION_PATH);
    let read_file = {
        let tmp = !Path::new(NVIM_PATH).exists() || !version_path.exists();
        if tmp { bprintln!("{$yellow+bold}No (valid) Neovim Installation Found.{/$}"); }
        !tmp
    };

    match runtime.block_on(run(client, version_path, read_file)) {
        Ok(_) => Success, Err(ex) => Fail(ex)
    }
}
