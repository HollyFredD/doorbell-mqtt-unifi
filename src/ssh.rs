use crate::config::SshConfig;

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::Path;

pub struct ExecOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct SshSession {
    session: ssh2::Session,
}

impl SshSession {
    /// TCP connect, handshake and password auth. The password is read from the
    /// environment variable named by `password_env`.
    pub fn connect(cfg: &SshConfig) -> Option<SshSession> {
        if cfg.host.is_empty() || cfg.user.is_empty() {
            log_error!("ssh session: invalid configuration");
            return None;
        }

        let Ok(port) = u16::try_from(cfg.port) else {
            log_error!("Invalid SSH port {}", cfg.port);
            return None;
        };

        let tcp = match TcpStream::connect((cfg.host.as_str(), port)) {
            Ok(t) => t,
            Err(e) => {
                log_error!("Failed to connect to {}:{}: {}", cfg.host, cfg.port, e);
                return None;
            }
        };

        let mut session = match ssh2::Session::new() {
            Ok(s) => s,
            Err(e) => {
                log_error!("libssh2 session init failed: {}", e);
                return None;
            }
        };

        session.set_tcp_stream(tcp);
        session.set_blocking(true);

        if let Err(e) = session.handshake() {
            log_error!("SSH handshake failed: {}", e);
            return None;
        }

        let password = std::env::var(&cfg.password_env).unwrap_or_default();
        if password.is_empty() {
            log_error!(
                "Environment variable '{}' is not set or empty. SSH Authentication cannot proceed.",
                cfg.password_env
            );
            return None;
        }

        if let Err(e) = session.userauth_password(&cfg.user, &password) {
            log_error!("Authentication failed for user '{}': {}", cfg.user, e);
            return None;
        }

        log_info!("Authenticated as '{}'", cfg.user);
        log_info!("SSH session established to {}:{}", cfg.host, cfg.port);

        Some(SshSession { session })
    }

    /// Runs a command and collects its exit status and output.
    /// None means the command could not run at all (channel/transport failure).
    pub fn exec(&self, command: &str) -> Option<ExecOutput> {
        let result = (|| -> io::Result<ExecOutput> {
            let mut channel = self.session.channel_session()?;
            channel.exec(command)?;

            // ponytail: stdout is drained before stderr; fine for the few lines these
            // commands print, interleave reads if a command ever floods stderr.
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            channel.read_to_end(&mut stdout)?;
            channel.stderr().read_to_end(&mut stderr)?;

            channel.wait_close()?;
            let status = channel.exit_status()?;

            Ok(ExecOutput {
                status,
                stdout: String::from_utf8_lossy(&stdout).into_owned(),
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            })
        })();

        match result {
            Ok(out) => Some(out),
            Err(e) => {
                log_error!("SSH exec failed for command '{}': {}", command, e);
                None
            }
        }
    }

    /// Runs a command whose exit status the caller does not act on: a non-zero
    /// status is only logged. Returns false when the command could not run.
    pub fn run(&self, command: &str) -> bool {
        match self.exec(command) {
            Some(out) => {
                if out.status != 0 {
                    log_error!("SSH command exited non-zero (exit={})", out.status);
                    log_debug!("Command: {}", command);
                }
                true
            }
            None => false,
        }
    }

    /// SCP upload to `remote_dir/<basename of local_path>`.
    pub fn upload(&self, local_path: &Path, remote_dir: &str, mode: i32) -> bool {
        let base = local_path
            .file_name()
            .map(|b| b.to_string_lossy().into_owned())
            .unwrap_or_default();
        let remote_path = format!("{remote_dir}/{base}");

        let result = (|| -> io::Result<()> {
            let mut file = File::open(local_path)?;
            let size = file.metadata()?.len();
            let mut channel = self
                .session
                .scp_send(Path::new(&remote_path), mode, size, None)?;

            io::copy(&mut file, &mut channel)?;
            channel.flush()?;
            channel.send_eof()?;
            channel.wait_eof()?;
            channel.wait_close()?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                log_info!(
                    "SCP upload complete: {} -> {}",
                    local_path.display(),
                    remote_path
                );
                true
            }
            Err(e) => {
                log_error!(
                    "SCP upload failed for '{}' -> '{}': {}",
                    local_path.display(),
                    remote_path,
                    e
                );
                false
            }
        }
    }

    pub fn download(&self, remote_path: &str, local_path: &Path) -> bool {
        let result = (|| -> io::Result<u64> {
            let (mut channel, stat) = self.session.scp_recv(Path::new(remote_path))?;
            let size = stat.size();

            let mut data = Vec::with_capacity(size as usize);
            (&mut channel).take(size).read_to_end(&mut data)?;

            if (data.len() as u64) < size {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("unexpected EOF (remaining={})", size - data.len() as u64),
                ));
            }

            // Older libssh2 versions could hand back the SCP protocol's trailing NUL as data
            if data.last() == Some(&0) {
                data.pop();
                log_debug!("Stripped trailing NUL from '{}'", remote_path);
            }

            File::create(local_path)?.write_all(&data)?;

            channel.send_eof()?;
            channel.wait_eof()?;
            channel.wait_close()?;
            Ok(size)
        })();

        match result {
            Ok(size) => {
                log_info!(
                    "SCP download complete: {} -> {} (expected={})",
                    remote_path,
                    local_path.display(),
                    size
                );
                true
            }
            Err(e) => {
                log_error!(
                    "SCP download failed for '{}' -> '{}': {}",
                    remote_path,
                    local_path.display(),
                    e
                );
                false
            }
        }
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        let _ = self.session.disconnect(None, "Normal shutdown", None);
    }
}

// ---- Remote commands ----

const SCRIPT_PREAMBLE: &str = r#"set -eu
STEP=""
fail() { rc=$1; echo "ERROR step=$STEP rc=$rc" 1>&2; exit $rc; }
run() { STEP=$1; shift; "$@" || fail $?; }

PERSIST_DIR='/etc/persistent'
ANIM_DIR='/etc/persistent/lcm/animation'
SND_DIR='/etc/persistent/sounds'
run ensure_dirs mkdir -p "$ANIM_DIR" "$SND_DIR"
"#;

const SCRIPT_RESTART: &str = r#"run restart_services sh -c '
  has_proc() { pidof "$1" >/dev/null 2>&1; }
  kill_ok()  { killall "$1" >/dev/null 2>&1; rc=$?; [ $rc -eq 0 -o $rc -eq 1 ] || exit $rc; }

  wait_both_gone() {
    t=$1; while [ $t -gt 0 ]; do
      has_proc ubnt_lcm_gui || a=0; has_proc ubnt_lcm_gui && a=1;
      has_proc ubnt_sounds_leds || b=0; has_proc ubnt_sounds_leds && b=1;
      [ $a -eq 0 -a $b -eq 0 ] && return 0;
      sleep 0.2; t=$((t-1));
    done; return 1;
  }

  wait_both_back() {
    t=$1; while [ $t -gt 0 ]; do
      has_proc ubnt_lcm_gui && a=1; has_proc ubnt_lcm_gui || a=0;
      has_proc ubnt_sounds_leds && b=1; has_proc ubnt_sounds_leds || b=0;
      [ $a -eq 1 -a $b -eq 1 ] && return 0;
      sleep 0.2; t=$((t-1));
    done; return 1;
  }

  kill_ok ubnt_lcm_gui
  kill_ok ubnt_sounds_leds

  # 0.2s * 25 = 5s max
  wait_both_gone 25 || exit 210
  # 0.2s * 50 = 10s max
  wait_both_back 50 || exit 211
  echo "OK"
'
"#;

pub const SCRIPT_STORAGE_GUARDRAIL: &str = concat!(
    "PERSIST_ROOT=/var/etc/persistent; ",
    "ANIM_DIR=\"$PERSIST_ROOT/lcm/animation\"; ",
    "SOUND_DIR=\"$PERSIST_ROOT/sounds\"; ",
    "STAGED_ROOT=/tmp/doorbell-mqtt-unifi/profile; ",
    "STAGED_ANIM_DIR=\"$STAGED_ROOT/anim\"; ",
    "STAGED_SOUND_DIR=\"$STAGED_ROOT/sound\"; ",
    "\n",
    "if [ ! -d \"$STAGED_ROOT\" ]; then ",
    "    echo \"ERROR: staged root $STAGED_ROOT does not exist\" >&2; ",
    "    exit 2; ",
    "fi; ",
    "\n",
    "if [ ! -d \"$STAGED_ANIM_DIR\" ] && [ ! -d \"$STAGED_SOUND_DIR\" ]; then ",
    "    echo \"ERROR: no staged assets found under $STAGED_ROOT\" >&2; ",
    "    exit 2; ",
    "fi; ",
    "\n",
    "free_now_kb=$(df -kP \"$PERSIST_ROOT\" | awk 'NR==2 {print $4}'); ",
    "\n",
    "old_assets_kb=$(du -sk \"$ANIM_DIR\" \"$SOUND_DIR\" 2>/dev/null | awk '{sum+=$1} END {print sum+0}'); ",
    "\n",
    "new_assets_kb=$(du -sk \"$STAGED_ANIM_DIR\" \"$STAGED_SOUND_DIR\" 2>/dev/null | awk '{sum+=$1} END {print sum+0}'); ",
    "\n",
    "projected_free_kb=$((free_now_kb + old_assets_kb - new_assets_kb)); ",
    "\n",
    "echo \"free_now_kb=$free_now_kb\"; ",
    "echo \"old_assets_kb=$old_assets_kb\"; ",
    "echo \"new_assets_kb=$new_assets_kb\"; ",
    "echo \"projected_free_kb=$projected_free_kb\"; ",
    "\n",
    "if [ \"$projected_free_kb\" -lt 1024 ]; then ",
    "    echo \"ERROR: projected free space would fall below 1024 KB\" >&2; ",
    "    exit 1; ",
    "fi; ",
    "\n",
    "if [ \"$projected_free_kb\" -lt 2048 ]; then ",
    "    echo \"WARN: projected free space below 2048 KB\"; ",
    "    exit 10; ",
    "fi; ",
    "\n",
    "echo \"OK: storage guardrail passed\"; ",
    "exit 0",
);

pub fn cmd_mkdir(path: &str) -> String {
    format!("mkdir -p '{path}'")
}

pub fn cmd_reset_dir(path: &str) -> String {
    format!("rm -rf '{path}' && mkdir -p '{path}'")
}

pub fn cmd_ipc_cli(target: &str, payload_path: &str) -> String {
    format!("ubnt_ipc_cli -T={target} -f=ubnt_ipc_cli -M={payload_path} -r=1 -F=- -z")
}

pub fn cmd_ipc_cli_cfg(target: &str, function: &str, config: &str) -> String {
    format!("ubnt_ipc_cli -T={target} -f=ubnt_ipc_cli -n={function} -p={config} -r=1 -F=- -z")
}

pub fn cmd_play_sfx(sfx_file_path: &str, volume: i32) -> String {
    format!(
        "/usr/local/bin/playSound.sh '{sfx_file_path}' -v {volume}; rc=$?; rm -f -- '{sfx_file_path}'; exit $rc"
    )
}

fn is_safe_single_quoted(s: &str) -> bool {
    !s.is_empty() && !s.contains(['\'', '\n', '\r'])
}

/// Moves staged animations (`<tmp>/anim/*.png` + md5) and sounds (`<tmp>/sound/*`)
/// into the persistent directories. Shared by both apply methods.
fn move_assets_script(tmp_dir: &str) -> String {
    format!(
        r#"# Only do animation work if there are any PNGs
set -- "{tmp_dir}/anim/"*.png
if [ -e "$1" ]; then
  run cleanup_anim rm -f -- "$ANIM_DIR"/*

  for f in "{tmp_dir}/anim/"*.png; do
    base=$(basename -- "$f")
    run move_anim mv -f -- "$f" "$ANIM_DIR/$base.anim"
  done

  # md5s optional
  set -- "{tmp_dir}/anim/"*.md5
  if [ -e "$1" ]; then
    run move_anim_md5 mv -f -- "{tmp_dir}/anim/"*.md5 "$ANIM_DIR/"
  fi
else
  echo "No animation assets; skipping"
fi
# Only do sound work if there are any files
set -- "{tmp_dir}/sound/"*
if [ -e "$1" ]; then
  run cleanup_snd rm -f -- "$SND_DIR"/*

  run move_snd mv -f -- "{tmp_dir}/sound/"* "$SND_DIR/"
else
  echo "No sound assets; skipping"
fi
"#
    )
}

/// Legacy apply: move assets, install the patched .conf files, restart the doorbell processes.
/// None when `tmp_dir` cannot be embedded in the script safely.
pub fn build_apply_profile_command(tmp_dir: &str) -> Option<String> {
    if !is_safe_single_quoted(tmp_dir) {
        return None;
    }

    Some(format!(
        "{SCRIPT_PREAMBLE}{}# Legacy apply: move patched conf files (always)\n\
         run move_anim_conf mv -f -- \"{tmp_dir}/ubnt_lcm_gui.conf.patched\" \"$PERSIST_DIR/ubnt_lcm_gui.conf\"\n\
         run move_snd_conf  mv -f -- \"{tmp_dir}/ubnt_sounds_leds.conf.patched\" \"$PERSIST_DIR/ubnt_sounds_leds.conf\"\n\
         {SCRIPT_RESTART}",
        move_assets_script(tmp_dir)
    ))
}

/// IPC apply, first step: move the staged assets in place (no restart).
pub fn build_move_assets_ipc(tmp_dir: &str) -> Option<String> {
    if !is_safe_single_quoted(tmp_dir) {
        return None;
    }

    Some(format!("{SCRIPT_PREAMBLE}{}", move_assets_script(tmp_dir)))
}

/// Last "ERROR step=<step> rc=<rc>" line printed by the scripts' `fail()`.
pub fn parse_step_error(stderr: &str) -> Option<(String, i32)> {
    let rest = &stderr[stderr.rfind("ERROR step=")? + "ERROR step=".len()..];
    let (step, rest) = rest.split_once(char::is_whitespace)?;
    let rc = rest.trim_start().strip_prefix("rc=")?;
    let end = rc
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(rc.len());

    Some((step.chars().take(63).collect(), rc[..end].parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: the C builders tested the first byte of an uninitialized output
    // buffer and failed at random. The command must always build for a safe path.
    #[test]
    fn apply_commands_build_for_safe_path() {
        let legacy = build_apply_profile_command("/tmp/doorbell-mqtt-unifi/profile").unwrap();
        assert!(legacy.starts_with("set -eu\n"));
        assert!(legacy.contains("\"/tmp/doorbell-mqtt-unifi/profile/ubnt_lcm_gui.conf.patched\""));
        assert!(legacy.contains("wait_both_back 50 || exit 211"));

        let ipc = build_move_assets_ipc("/tmp/doorbell-mqtt-unifi/profile").unwrap();
        assert!(ipc.contains(
            "run move_snd mv -f -- \"/tmp/doorbell-mqtt-unifi/profile/sound/\"* \"$SND_DIR/\""
        ));
        assert!(!ipc.contains("restart_services"));
    }

    #[test]
    fn apply_commands_reject_unsafe_path() {
        assert!(build_apply_profile_command("/tmp/x'; reboot; '").is_none());
        assert!(build_move_assets_ipc("/tmp/x\nreboot").is_none());
        assert!(build_move_assets_ipc("").is_none());
    }

    #[test]
    fn parses_last_step_error() {
        let stderr = "noise\nERROR step=move_anim rc=1\nERROR step=move_snd rc=28\n";
        assert_eq!(parse_step_error(stderr), Some(("move_snd".to_string(), 28)));
        assert_eq!(parse_step_error("all good"), None);
    }
}
