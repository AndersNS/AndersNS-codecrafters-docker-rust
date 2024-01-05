use anyhow::{Context, Result};
use flate2::bufread::GzDecoder;
use reqwest::header::ACCEPT;
use std::{
    os::unix::fs,
    path::{Path, PathBuf},
    process::Stdio,
};

#[derive(serde::Deserialize, Debug)]
struct AuthResp {
    token: String,
}

#[derive(serde::Deserialize, Debug)]
struct DistributionManifestResponse {
    manifests: Vec<DistributionManifest>,
}

#[derive(serde::Deserialize, Debug)]
struct DistributionManifest {
    digest: String,
    platform: Platform,
}

#[derive(serde::Deserialize, Debug)]
struct Platform {
    architecture: String,
}

#[derive(serde::Deserialize, Debug)]
struct ImageManifestResponse {
    layers: Vec<Layer>,
}

#[derive(serde::Deserialize, Debug)]
struct Layer {
    digest: String,
}

fn create_temp_dir() -> Result<PathBuf> {
    // Create temp dir
    let temp_dir = tempfile::tempdir()?;
    let temp_dir_path = temp_dir.into_path();

    // Because of some weirdness with chroot, we need to create the dev/null file
    std::fs::create_dir_all(temp_dir_path.join("dev"))?;
    std::fs::File::create(temp_dir_path.join("dev/null"))?;

    Ok(temp_dir_path)
}

fn chroot_to_temp_dir(temp_dir_path: &Path) -> Result<()> {
    fs::chroot(temp_dir_path)?;
    std::env::set_current_dir("/")?;

    Ok(())
}

async fn get_auth_token(image: &str) -> Result<String, anyhow::Error> {
    let auth_res = reqwest::get(format!(
        "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/{}:pull",
        image
    ))
    .await?
    .json::<AuthResp>()
    .await?;

    Ok(auth_res.token)
}

async fn get_image_digest(
    client: &reqwest::Client,
    image: &str,
    tag: &str,
    token: &str,
    platform_architecture: &str,
) -> Result<String, anyhow::Error> {
    let manifest: DistributionManifestResponse = client
        .get(format!(
            "https://registry.hub.docker.com/v2/library/{image}/manifests/{tag}",
            image = image,
            tag = tag
        ))
        .header(
            ACCEPT,
            "application/vnd.docker.distribution.manifest.list.v2+json",
        )
        .bearer_auth(token)
        .send()
        .await?
        .json()
        .await?;
    let image_digest = &manifest
        .manifests
        .iter()
        .find(|m| m.platform.architecture == platform_architecture)
        .context("No manifest found for arm64")?
        .digest;

    Ok(image_digest.to_owned())
}

async fn get_image_layers(
    client: &reqwest::Client,
    image: &str,
    image_digest: &str,
    token: &str,
) -> Result<Vec<Layer>, anyhow::Error> {
    let image_manifest: ImageManifestResponse = client
        .get(format!(
            "https://registry.hub.docker.com/v2/library/{image}/manifests/{digest}",
            image = image,
            digest = image_digest
        ))
        .header(ACCEPT, "application/vnd.oci.image.manifest.v1+json")
        .bearer_auth(token)
        .send()
        .await?
        .json()
        .await?;

    Ok(image_manifest.layers)
}

async fn download_layers(
    client: &reqwest::Client,
    token: &str,
    image: &str,
    layers: Vec<Layer>,
    temp_dir_path: &Path,
) -> Result<(), anyhow::Error> {
    for layer in layers {
        let layer_data = client
            .get(format!(
                "https://registry.hub.docker.com/v2/library/{image}/blobs/{digest}",
                image = image,
                digest = layer.digest
            ))
            .bearer_auth(token)
            .send()
            .await?
            .bytes()
            .await?;

        let gzip_decoder = GzDecoder::new(layer_data.as_ref());
        tar::Archive::new(gzip_decoder).unpack(temp_dir_path)?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let image_name = &args[2];
    let command = &args[3];
    let command_args = &args[4..];

    let temp_dir_path = create_temp_dir()?;

    let mut split = image_name.split(':');
    let image = split.next().unwrap();
    let tag = split.next().unwrap_or("latest");

    // Get docker registry token
    let token = get_auth_token(image).await?;

    // Get the manifest for this image distribution
    let client = reqwest::Client::new();

    // Get the image digest (id) for arm64
    let image_digest = get_image_digest(&client, image, tag, &token, "arm64").await?;

    // Download layers from docker registry
    let image_layers = get_image_layers(&client, image, &image_digest, &token).await?;
    // Download each layer and unpack it to the temp dir
    download_layers(&client, &token, image, image_layers, &temp_dir_path).await?;

    // Scope to the temp dir with chroot
    chroot_to_temp_dir(&temp_dir_path)?;

    // HACK: Doesn't compile on macOS, run this program on Linux via docker
    unsafe { libc::unshare(libc::CLONE_NEWPID) };

    // Run the command
    let output = std::process::Command::new(command)
        .current_dir("/")
        .args(command_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_clear()
        .output()
        .with_context(|| format!("Tried to run '{}' ", command,))?;

    if output.status.success() {
        let std_out = std::str::from_utf8(&output.stdout)?;
        let std_err = std::str::from_utf8(&output.stderr)?;
        print!("{}", std_out);
        eprint!("{}", std_err);
    } else {
        std::process::exit(output.status.code().unwrap_or(1))
    }

    Ok(())
}
