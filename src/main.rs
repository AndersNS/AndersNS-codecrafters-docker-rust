use anyhow::{Context, Result};
use std::{os::unix::fs, path::Path, process::Stdio};

// Usage: your_docker.sh run <image> <command> <arg1> <arg2> ...
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    let command = &args[3];
    let command_args = &args[4..];

    // Create temp dir
    let temp_dir = tempfile::tempdir()?;
    let temp_dir_path = temp_dir.into_path();

    // Because of some weirdness with chroot, we need to create dev/null
    std::fs::create_dir_all(temp_dir_path.join("dev"))?;
    std::fs::File::create(temp_dir_path.join("dev/null"))?;

    let command_path = Path::new(command)
        .file_name()
        .context("Command is an invalid filename?")?;
    std::fs::copy(Path::new(command), temp_dir_path.join(command_path))
        .context("Could not copy command to temp directory")?;

    fs::chroot(temp_dir_path.as_path())?;
    std::env::set_current_dir("/")?;
    let command_call = Path::new("/").join(command_path);

    let output = std::process::Command::new(command_call)
        .current_dir("/")
        .args(command_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_clear()
        .output()
        .with_context(|| {
            format!(
                "Tried to run '{}' with arguments {:?}, in temp file {:?}",
                command_path.to_str().unwrap(),
                command_args,
                temp_dir_path
            )
        })?;

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
