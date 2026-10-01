//! Session-local indirection for SSH agents whose sockets belong to an attachment.

use std::fs;
use std::io;
use std::os::unix::fs::{symlink, FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use interprocess::local_socket::{ConnectOptions, GenericFilePath, ToFsName};
use interprocess::ConnectWaitMode;

use crate::ipc::LocalStream;

const PROBE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(crate) struct SshAgentRegistry(Arc<Mutex<State>>);

struct State {
    path: PathBuf,
    fallback: Option<PathBuf>,
    agents: Vec<(u64, PathBuf)>,
    next_id: u64,
    identity: Option<(u64, u64)>,
    last_probe: Option<Instant>,
}

pub(crate) struct SshAgentLease {
    registry: SshAgentRegistry,
    id: u64,
}

pub(crate) fn socket_path() -> PathBuf {
    agent_path_for(&crate::api::socket_path())
}

fn agent_path_for(api_path: &Path) -> PathBuf {
    let mut path = api_path.as_os_str().to_os_string();
    path.push(".agent");
    path.into()
}

fn usable_socket(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| {
        // The API is user-private; do not redirect that user's panes to another user's agent.
        metadata.file_type().is_socket() && metadata.uid() == unsafe { libc::geteuid() }
    })
}

fn live_socket(path: &Path) -> bool {
    if !usable_socket(path) {
        return false;
    }
    let Ok(name) = path.to_fs_name::<GenericFilePath>() else {
        return false;
    };
    // Never wait for a full accept queue or retain a forwarded SSH channel after probing.
    let Ok(LocalStream::UdSocket(stream)) = ConnectOptions::new()
        .name(name)
        .wait_mode(ConnectWaitMode::Timeout(Duration::ZERO))
        .nonblocking_stream(true)
        .connect_sync()
    else {
        return false;
    };
    // Linux can report an unconnected socket writable after connect returns EAGAIN.
    stream.inner().peer_addr().is_ok()
}

impl SshAgentRegistry {
    pub(crate) fn new(path: PathBuf, inherited: Option<PathBuf>) -> io::Result<Self> {
        let inherited = inherited.filter(|path| !path.as_os_str().is_empty());
        let managed = inherited.is_some()
            || fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink());
        // A handoff keeps existing pane environments, including this stable pathname.
        let fallback = fs::read_link(&path)
            .ok()
            .filter(|target| target != &path && usable_socket(target))
            .or_else(|| inherited.filter(|target| target != &path && usable_socket(target)));
        let mut state = State {
            path,
            fallback,
            agents: Vec::new(),
            next_id: 0,
            identity: None,
            last_probe: None,
        };
        if managed {
            state.publish()?;
        }
        Ok(Self(Arc::new(Mutex::new(state))))
    }

    pub(crate) fn register(&self, path: PathBuf) -> io::Result<SshAgentLease> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| io::Error::other("SSH agent registry poisoned"))?;
        if !path.is_absolute() || path == state.path || !usable_socket(&path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SSH agent must be an absolute, user-owned socket",
            ));
        }
        // Canonicalizing /var to /private/var can exceed macOS's Unix socket path limit.
        let id = state.next_id;
        state.next_id += 1;
        state.agents.push((id, path));
        if let Err(error) = state.publish() {
            state.agents.retain(|(candidate, _)| *candidate != id);
            return Err(error);
        }
        Ok(SshAgentLease {
            registry: self.clone(),
            id,
        })
    }
}

impl State {
    fn publish(&mut self) -> io::Result<()> {
        if let Some(identity) = self.identity {
            let metadata = fs::symlink_metadata(&self.path)?;
            if identity != (metadata.dev(), metadata.ino()) {
                return Err(io::Error::other(
                    "SSH agent address belongs to a replacement server",
                ));
            }
        }
        // Keep a working agent rather than letting probes or a second client replace it.
        let unavailable = self.path.with_extension("unavailable");
        self.last_probe = Some(Instant::now());
        let target = self
            .fallback
            .as_deref()
            .filter(|path| live_socket(path))
            .or_else(|| {
                self.agents
                    .iter()
                    .map(|(_, path)| path.as_path())
                    .find(|path| live_socket(path))
            })
            .unwrap_or(&unavailable);
        if self.identity.is_some() && fs::read_link(&self.path).ok().as_deref() == Some(target) {
            return Ok(());
        }
        let temporary = self
            .path
            .with_extension(format!("{}.new", std::process::id()));
        symlink(target, &temporary)?;
        if let Err(error) = fs::rename(&temporary, &self.path) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let metadata = fs::symlink_metadata(&self.path)?;
        self.identity = Some((metadata.dev(), metadata.ino()));
        Ok(())
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| self.identity == Some((metadata.dev(), metadata.ino())))
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl SshAgentLease {
    pub(crate) fn refresh(&self) -> io::Result<()> {
        self.refresh_at(Instant::now())
    }

    fn refresh_at(&self, now: Instant) -> io::Result<()> {
        let mut state = self
            .registry
            .0
            .lock()
            .map_err(|_| io::Error::other("SSH agent registry poisoned"))?;
        // Share the probe budget across attachments, not one SSH channel per polling client.
        if state
            .last_probe
            .is_none_or(|last| now.saturating_duration_since(last) >= PROBE_INTERVAL)
        {
            state.publish()?;
        }
        Ok(())
    }
}

impl Drop for SshAgentLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.registry.0.lock() {
            state.agents.retain(|(id, _)| *id != self.id);
            if let Err(error) = state.publish() {
                tracing::warn!(%error, "could not refresh SSH agent after attachment ended");
            }
        }
    }
}

pub(crate) fn apply_pane_env(command: &mut portable_pty::CommandBuilder) {
    let path = socket_path();
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        command.env("SSH_AUTH_SOCK", path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn server_without_an_agent_leaves_local_pane_agent_setup_alone() {
        let directory = std::env::temp_dir().join(format!("herdr-no-agent-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let stable = directory.join("agent");
        for inherited in [None, Some(PathBuf::new())] {
            let registry = SshAgentRegistry::new(stable.clone(), inherited).unwrap();
            assert!(
                fs::symlink_metadata(&stable).is_err(),
                "a local server without an agent must not advertise an agent address to panes"
            );
            drop(registry);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn closed_agent_listener_does_not_block_a_live_replacement() {
        let directory =
            std::env::temp_dir().join(format!("herdr-dead-agent-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let stable = directory.join("agent");
        let a = directory.join("a");
        let b = directory.join("b");
        let listener_a = UnixListener::bind(&a).unwrap();
        let _listener_b = UnixListener::bind(&b).unwrap();
        let registry = SshAgentRegistry::new(stable.clone(), Some(a.clone())).unwrap();
        let (mut probe, _) = listener_a.accept().unwrap();
        probe.set_nonblocking(true).unwrap();
        assert_eq!(
            std::io::Read::read(&mut probe, &mut [0]).unwrap(),
            0,
            "agent checks must not hold forwarded SSH channels open"
        );
        let lease_b = registry.register(b.clone()).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), a);
        drop(listener_a);
        assert!(fs::metadata(&a).unwrap().file_type().is_socket());
        lease_b.refresh_at(Instant::now() + PROBE_INTERVAL).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), b);
        drop(lease_b);
        drop(registry);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unestablished_agent_connection_does_not_block_a_live_replacement() {
        use std::os::fd::AsRawFd;
        use std::os::unix::net::UnixStream;
        let directory =
            std::env::temp_dir().join(format!("herdr-busy-agent-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let a = directory.join("a");
        let b = directory.join("b");
        let stable = directory.join("agent");
        let listener_a = UnixListener::bind(&a).unwrap();
        // Linux allows one queued connection with a zero backlog.
        assert_eq!(unsafe { libc::listen(listener_a.as_raw_fd(), 0) }, 0);
        let _queued = UnixStream::connect(&a).unwrap();
        let _listener_b = UnixListener::bind(&b).unwrap();
        let registry = SshAgentRegistry::new(stable.clone(), Some(a)).unwrap();
        let lease_b = registry.register(b.clone()).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), b);
        drop(lease_b);
        drop(registry);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn registration_preserves_the_supplied_socket_address() {
        let directory =
            std::env::temp_dir().join(format!("herdr-agent-path-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        symlink(".", directory.join("alias")).unwrap();
        let _listener = UnixListener::bind(directory.join("upstream")).unwrap();
        let supplied = directory.join("alias/upstream");
        let stable = directory.join("agent");
        let registry = SshAgentRegistry::new(stable.clone(), None).unwrap();
        let lease = registry.register(supplied.clone()).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), supplied);
        drop(lease);
        drop(registry);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn socket_overrides_have_independent_agent_addresses() {
        let directory =
            std::env::temp_dir().join(format!("herdr-agent-overrides-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let a = directory.join("a");
        let b = directory.join("b");
        let _a_listener = UnixListener::bind(&a).unwrap();
        let _b_listener = UnixListener::bind(&b).unwrap();
        let stable_a = agent_path_for(&directory.join("first.sock"));
        let stable_b = agent_path_for(&directory.join("second.sock"));
        let registry_a = SshAgentRegistry::new(stable_a.clone(), Some(a.clone())).unwrap();
        let registry_b = SshAgentRegistry::new(stable_b.clone(), Some(b.clone())).unwrap();
        assert_eq!(fs::read_link(&stable_a).unwrap(), a);
        assert_eq!(fs::read_link(&stable_b).unwrap(), b);
        drop(registry_a);
        assert_eq!(fs::read_link(&stable_b).unwrap(), b);
        drop(registry_b);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reconnect_and_overlapping_attachments_keep_a_stable_agent_address() {
        let directory =
            std::env::temp_dir().join(format!("herdr-ssh-agent-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let stable = directory.join("agent");
        let a = directory.join("a");
        let b = directory.join("b");
        let probe = directory.join("probe");
        let _a_listener = UnixListener::bind(&a).unwrap();
        let _b_listener = UnixListener::bind(&b).unwrap();
        let _probe_listener = UnixListener::bind(&probe).unwrap();
        let registry = SshAgentRegistry::new(stable.clone(), Some(a.clone())).unwrap();
        let temporary = registry.register(a.clone()).unwrap();
        drop(temporary);
        assert_eq!(fs::read_link(&stable).unwrap(), a);
        let lease_a = registry.register(a.clone()).unwrap();
        let lease_probe = registry.register(probe).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), a);
        drop(lease_probe);
        let lease_b = registry.register(b.clone()).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), a);
        fs::remove_file(&a).unwrap();
        lease_b.refresh_at(Instant::now() + PROBE_INTERVAL).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), b);
        drop(lease_a);
        assert_eq!(fs::read_link(&stable).unwrap(), b);
        drop(lease_b);
        assert!(!stable.exists());
        assert!(fs::symlink_metadata(&stable).is_ok());
        let _lease_b = registry.register(b.clone()).unwrap();
        assert_eq!(fs::read_link(&stable).unwrap(), b);
        assert!(registry.register(stable).is_err());
        drop(_lease_b);
        drop(registry);
        fs::remove_dir_all(directory).unwrap();
    }
}
