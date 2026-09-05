//! Generates a systemd **user** unit (no root needed) that runs
//! `gaggle-accelerator-launcher run` — printed to stdout by default, or
//! written straight to `~/.config/systemd/user/` with `--install`. This is
//! the piece that makes "auto-updates on restart" concrete: `Restart=always`
//! means every restart re-runs this launcher's update-then-exec, so the
//! daemon it execs is always brought current first.

use std::path::Path;

use anyhow::{Context, Result};
use clap::Args;

#[derive(Debug, Args)]
pub struct ServiceArgs {
    /// `--role` to bake into the generated `ExecStart` line.
    #[arg(long, default_value = "relay")]
    role: String,
    /// `--listen` multiaddr to bake into the generated `ExecStart` line.
    #[arg(long)]
    listen: Option<String>,
    /// `--admin-listen` host:port to bake into the generated `ExecStart` line.
    #[arg(long)]
    admin_listen: Option<String>,
    /// Write to `~/.config/systemd/user/gaggle-accelerator.service` instead of
    /// printing to stdout.
    #[arg(long)]
    install: bool,
}

pub fn run(args: ServiceArgs) -> Result<()> {
    // Wherever this binary is currently running from — put it at a stable
    // path (e.g. `/usr/local/bin` or `~/.local/bin`) before generating a unit
    // that points at it; a systemd `ExecStart=` needs a permanent path, same
    // as for any other service.
    let launcher_bin = std::env::current_exe().context("resolve this binary's own path")?;
    let unit = unit_file(&launcher_bin, &args.role, args.listen.as_deref(), args.admin_listen.as_deref());

    if !args.install {
        print!("{unit}");
        return Ok(());
    }

    let dir = dirs::config_dir().context("no OS config directory")?.join("systemd").join("user");
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join("gaggle-accelerator.service");
    std::fs::write(&path, &unit).with_context(|| format!("write {}", path.display()))?;

    println!("wrote {}", path.display());
    println!();
    println!("Next steps:");
    println!("  systemctl --user daemon-reload");
    println!("  systemctl --user enable --now gaggle-accelerator.service");
    println!("  loginctl enable-linger $USER   # so it runs without staying logged in");
    Ok(())
}

fn unit_file(launcher_bin: &Path, role: &str, listen: Option<&str>, admin_listen: Option<&str>) -> String {
    let mut accel_args = format!("run --role {role}");
    if let Some(l) = listen {
        accel_args.push_str(&format!(" --listen {l}"));
    }
    if let Some(a) = admin_listen {
        accel_args.push_str(&format!(" --admin-listen {a}"));
    }
    format!(
        "[Unit]\n\
         Description=Gaggle accelerator (auto-updating)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart=\"{bin}\" run -- {accel_args}\n\
         Restart=always\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        bin = launcher_bin.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_file_has_required_sections_and_execstart() {
        let text = unit_file(
            Path::new("/usr/local/bin/gaggle-accelerator-launcher"),
            "nas",
            Some("/ip4/0.0.0.0/udp/4001/quic-v1"),
            Some("0.0.0.0:8749"),
        );
        assert!(text.starts_with("[Unit]\n"));
        assert!(text.contains("\n[Service]\n"));
        assert!(text.contains("\n[Install]\n"));
        assert!(text.contains("Restart=always\n"));
        assert!(text.contains(
            "ExecStart=\"/usr/local/bin/gaggle-accelerator-launcher\" run -- run --role nas \
             --listen /ip4/0.0.0.0/udp/4001/quic-v1 --admin-listen 0.0.0.0:8749\n"
        ));
    }

    #[test]
    fn unit_file_omits_unset_optional_flags() {
        let text = unit_file(Path::new("/bin/gaggle-accelerator-launcher"), "relay", None, None);
        assert!(text.contains("run -- run --role relay\n"));
        assert!(!text.contains("--listen"));
        assert!(!text.contains("--admin-listen"));
    }
}
